# Defines the opinionated NixOS service boundary for Telchar, PostgreSQL, gateway-store access, and restricted SSH ingress.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.telchar;
  sshRenewalCfg = cfg.sshHostCertificateRenewal;
  nomadRenewalCfg = cfg.nomad.renewal;
  vaultCfg = cfg.vaultAwsAuth;
  vaultPython = pkgs.python3.withPackages (pythonPackages: [ pythonPackages.botocore ]);
  toml = pkgs.formats.toml { };
  protectedDatabase = cfg.database.urlFile != null;
  serviceSettings = lib.recursiveUpdate cfg.settings (
    lib.optionalAttrs protectedDatabase {
      database.url_file = cfg.database.urlFile;
    }
    // lib.optionalAttrs cfg.callback.enable {
      backends.nomad_callback.bind = "${cfg.callback.bindAddress}:${toString cfg.callback.port}";
    }
  );
  configurationFile = toml.generate "telchar.toml" serviceSettings;
  credentialFiles = map (credential: "${credential.name}:${credential.source}") cfg.credentials;
  daemonEnvironment =
    if protectedDatabase then
      removeAttrs cfg.environment [ "TELCHAR_DATABASE_URL" ]
    else
      cfg.environment;
  databaseValidator = pkgs.writeShellScript "validate-telchar-database-url" ''
    exec ${cfg.package}/bin/telchar validate-database-tls \
      ${lib.escapeShellArg cfg.database.urlFile} \
      ${lib.escapeShellArg cfg.database.rootCertificateFile}
  '';
  renewHostCertificate = pkgs.writeShellScript "renew-telchar-ssh-host-certificate" ''
    set -eu
    candidate=${lib.escapeShellArg sshRenewalCfg.candidateFile}
    destination=${lib.escapeShellArg cfg.ingress.openssh.hostCertificateFile}
    host_public_key=${lib.escapeShellArg "${cfg.ingress.openssh.hostKeyFile}.pub"}
    certificate="$(${pkgs.openssh}/bin/ssh-keygen -L -f "$candidate")"
    candidate_fingerprint="$(${pkgs.openssh}/bin/ssh-keygen -lf "$candidate" | ${pkgs.gawk}/bin/awk '{ print $2 }')"
    host_fingerprint="$(${pkgs.openssh}/bin/ssh-keygen -lf "$host_public_key" | ${pkgs.gawk}/bin/awk '{ print $2 }')"
    signing_ca_fingerprint="$(${pkgs.openssh}/bin/ssh-keygen -lf ${lib.escapeShellArg sshRenewalCfg.expectedSigningCAFile} | ${pkgs.gawk}/bin/awk '{ print $2 }')"
    test "$candidate_fingerprint" = "$host_fingerprint"
    certificate_type="$(printf '%s\n' "$certificate" | ${pkgs.gawk}/bin/awk '/^[[:space:]]*Type:/{sub(/^[[:space:]]*Type:[[:space:]]*/, ""); print; exit}')"
    certificate_signing_ca="$(printf '%s\n' "$certificate" | ${pkgs.gawk}/bin/awk '/^[[:space:]]*Signing CA:/{for (field = 1; field <= NF; field++) if ($field ~ /^SHA256:/) { print $field; exit }}')"
    test "$certificate_type" = "ssh-ed25519-cert-v01@openssh.com host certificate"
    test "$certificate_signing_ca" = "$signing_ca_fingerprint"
    principals="$(printf '%s\n' "$certificate" | ${pkgs.gawk}/bin/awk '/^[[:space:]]*Principals:/{inside=1; next} inside && /^[[:space:]]*Critical Options:/{exit} inside {sub(/^[[:space:]]+/, ""); if (length) print}')"
    ${lib.concatMapStringsSep "\n    " (
      principal:
      ''printf '%s\n' "$principals" | ${pkgs.gnugrep}/bin/grep -F -x -q ${lib.escapeShellArg principal}''
    ) sshRenewalCfg.expectedPrincipals}
    valid_from="$(printf '%s\n' "$certificate" | ${pkgs.gawk}/bin/awk '/^[[:space:]]*Valid: from /{print $3; exit}')"
    valid_to="$(printf '%s\n' "$certificate" | ${pkgs.gawk}/bin/awk '/^[[:space:]]*Valid: from /{print $5; exit}')"
    now="$(${pkgs.coreutils}/bin/date +%s)"
    valid_from_epoch="$(${pkgs.coreutils}/bin/date -d "$valid_from" +%s)"
    valid_to_epoch="$(${pkgs.coreutils}/bin/date -d "$valid_to" +%s)"
    test "$valid_from_epoch" -le "$now"
    test "$valid_to_epoch" -ge "$((now + ${toString sshRenewalCfg.minimumRemainingValiditySec}))"
    temporary="$(${pkgs.coreutils}/bin/mktemp "$(dirname "$destination")/.host-certificate.XXXXXX")"
    trap '${pkgs.coreutils}/bin/rm -f "$temporary"' EXIT
    ${pkgs.coreutils}/bin/install -m 0644 "$candidate" "$temporary"
    ${pkgs.coreutils}/bin/mv -f "$temporary" "$destination"
    trap - EXIT
  '';
  reloadSshIngress = pkgs.writeShellScript "reload-telchar-ssh-ingress" ''
    if ${pkgs.systemd}/bin/systemctl is-active --quiet telchar-sshd.service; then
      ${pkgs.systemd}/bin/systemctl reload telchar-sshd.service
    fi
  '';
  renewNomadToken = pkgs.writeShellScript "renew-telchar-nomad-token" ''
    set -eu
    candidate=${lib.escapeShellArg nomadRenewalCfg.candidateFile}
    destination=${lib.escapeShellArg cfg.nomad.tokenFile}
    test -f "$candidate"
    test ! -L "$candidate"
    test -s "$candidate"
    mode="$(${pkgs.coreutils}/bin/stat -c %a "$candidate")"
    test "$((0$mode & 077))" -eq 0
    temporary="$(${pkgs.coreutils}/bin/mktemp "$(dirname "$destination")/.nomad-token.XXXXXX")"
    trap '${pkgs.coreutils}/bin/rm -f "$temporary"' EXIT
    ${pkgs.coreutils}/bin/install -m 0400 "$candidate" "$temporary"
    ${pkgs.coreutils}/bin/mv -f "$temporary" "$destination"
    trap - EXIT
  '';
  reloadTelchar = pkgs.writeShellScript "reload-telchar" ''
    if ${pkgs.systemd}/bin/systemctl is-active --quiet telchar.service; then
      ${pkgs.systemd}/bin/systemctl kill --kill-whom=main --signal=HUP telchar.service
    fi
  '';
  fetchVaultCandidates = pkgs.writeText "fetch-telchar-vault-candidates.py" ''
    import base64
    import grp
    import json
    import os
    import pwd
    import tempfile
    import time
    import urllib.error
    import urllib.parse
    import urllib.request

    from botocore.auth import SigV4Auth
    from botocore.awsrequest import AWSRequest
    from botocore.credentials import Credentials

    metadata_endpoint = ${builtins.toJSON vaultCfg.metadataEndpoint}
    vault_address = ${builtins.toJSON vaultCfg.address}
    request_timeout_seconds = 2
    request_attempts = 3
    maximum_response_bytes = 1024 * 1024

    def bounded_request(request):
        for attempt in range(request_attempts):
            try:
                with urllib.request.urlopen(request, timeout=request_timeout_seconds) as response:
                    content_length = response.headers.get("Content-Length")
                    if content_length is not None and int(content_length) > maximum_response_bytes:
                        raise ValueError("response exceeds maximum size")
                    body = response.read(maximum_response_bytes + 1)
                    if len(body) > maximum_response_bytes:
                        raise ValueError("response exceeds maximum size")
                    return body
            except urllib.error.HTTPError as error:
                if error.code < 500 and error.code != 429:
                    raise
                if attempt + 1 == request_attempts:
                    raise
            except (TimeoutError, urllib.error.URLError):
                if attempt + 1 == request_attempts:
                    raise
            time.sleep(0.25 * (attempt + 1))

    def json_request(request):
        return json.loads(bounded_request(request))
    metadata_token_request = urllib.request.Request(
        metadata_endpoint + "/latest/api/token",
        method="PUT",
        headers={"X-aws-ec2-metadata-token-ttl-seconds": "21600"},
    )
    metadata_token = bounded_request(metadata_token_request).decode()

    metadata_headers = {"X-aws-ec2-metadata-token": metadata_token}
    role_request = urllib.request.Request(
        metadata_endpoint + "/latest/meta-data/iam/security-credentials/",
        headers=metadata_headers,
    )
    instance_role = bounded_request(role_request).decode().strip()

    credentials_request = urllib.request.Request(
        metadata_endpoint + "/latest/meta-data/iam/security-credentials/" + urllib.parse.quote(instance_role),
        headers=metadata_headers,
    )
    credential_document = json_request(credentials_request)

    credentials = Credentials(
        credential_document["AccessKeyId"],
        credential_document["SecretAccessKey"],
        credential_document["Token"],
    )
    sts_url = "https://sts.amazonaws.com/"
    sts_body = "Action=GetCallerIdentity&Version=2011-06-15"
    aws_request = AWSRequest(
        method="POST",
        url=sts_url,
        data=sts_body,
        headers={"Content-Type": "application/x-www-form-urlencoded; charset=utf-8"},
    )
    SigV4Auth(credentials, "sts", "us-east-1").add_auth(aws_request)
    prepared_request = aws_request.prepare()
    login_payload = {
        "role": ${builtins.toJSON vaultCfg.role},
        "iam_http_request_method": "POST",
        "iam_request_url": base64.b64encode(sts_url.encode()).decode(),
        "iam_request_body": base64.b64encode(sts_body.encode()).decode(),
        "iam_request_headers": base64.b64encode(json.dumps(dict(prepared_request.headers)).encode()).decode(),
    }

    def vault_request(path, method="GET", payload=None, token=None):
        headers = {"Content-Type": "application/json"}
        if token is not None:
            headers["X-Vault-Token"] = token
        body = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(
            vault_address + "/v1/" + path.lstrip("/"),
            data=body,
            method=method,
            headers=headers,
        )
        return json_request(request)

    login = vault_request(
        "auth/${vaultCfg.authMount}/login",
        method="POST",
        payload=login_payload,
    )
    vault_token = login["auth"]["client_token"]
    with open(${builtins.toJSON "${cfg.ingress.openssh.hostKeyFile}.pub"}) as host_public_key:
        signed_certificate = vault_request(
            ${builtins.toJSON vaultCfg.sshSignPath},
            method="POST",
            payload={"public_key": host_public_key.read()},
            token=vault_token,
        )["data"]["signed_key"]
    nomad_secret = vault_request(
        ${builtins.toJSON vaultCfg.nomadSecretPath},
        token=vault_token,
    )["data"]
    nomad_token = nomad_secret.get("secret_id")
    if nomad_token is None:
        nomad_token = nomad_secret["data"]["token"]

    telchar_uid = pwd.getpwnam(${builtins.toJSON cfg.user}).pw_uid
    telchar_gid = grp.getgrnam(${builtins.toJSON cfg.group}).gr_gid

    def write_candidate(path, content):
        directory = os.path.dirname(path)
        os.makedirs(directory, mode=0o700, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(prefix=".telchar-candidate.", dir=directory, text=True)
        try:
            with os.fdopen(descriptor, "w") as output:
                output.write(content)
                output.flush()
                os.fsync(output.fileno())
            os.chmod(temporary, 0o400)
            os.replace(temporary, path)
        except BaseException:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass
            raise

    write_candidate(${builtins.toJSON sshRenewalCfg.candidateFile}, signed_certificate)
    write_candidate(${builtins.toJSON nomadRenewalCfg.candidateFile}, nomad_token)
  '';
  forcedCommand = pkgs.writeShellScript "telchar-forced-command" ''
    set -eu
    : "''${SSH_USER_AUTH:?OpenSSH authentication metadata is unavailable}"
    authenticated_key="$(${pkgs.gawk}/bin/awk '$1 == "publickey" { print $2, $3; exit }' "$SSH_USER_AUTH")"
    if [ -z "$authenticated_key" ]; then
      echo "OpenSSH public-key identity is unavailable" >&2
      exit 1
    fi
    fingerprint="$(printf '%s\n' "$authenticated_key" | ${pkgs.openssh}/bin/ssh-keygen -lf - | ${pkgs.gawk}/bin/awk '{ print $2 }')"
    exec env \
      TELCHAR_IPC_SOCKET=${lib.escapeShellArg cfg.socketPath} \
      TELCHAR_AUTHENTICATED_KEY="$fingerprint" \
      ${cfg.package}/bin/telchar serve-stdio
  '';
  certificateIngress = cfg.ingress.openssh.hostCertificateFile != null;
  sshdConfiguration = pkgs.writeText "telchar-sshd_config" ''
    Port ${toString cfg.ingress.openssh.port}
    ${lib.concatMapStringsSep "\n" (
      address: "ListenAddress ${address}"
    ) cfg.ingress.openssh.listenAddresses}
    HostKey ${cfg.ingress.openssh.hostKeyFile}
    ${lib.optionalString certificateIngress "HostCertificate ${cfg.ingress.openssh.hostCertificateFile}"}
    ${lib.optionalString certificateIngress "TrustedUserCAKeys ${cfg.ingress.openssh.trustedUserCAKeysFile}"}
    ${lib.optionalString certificateIngress "AuthorizedPrincipalsFile ${cfg.ingress.openssh.authorizedPrincipalsFile}"}
    PidFile /run/telchar-sshd/sshd.pid
    AuthorizedKeysFile ${cfg.ingress.openssh.authorizedKeysFile}
    AuthenticationMethods publickey
    PubkeyAuthentication yes
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    PermitRootLogin no
    PermitEmptyPasswords no
    StrictModes yes
    UsePAM no
    AllowUsers ${cfg.user}
    ForceCommand ${forcedCommand}
    ExposeAuthInfo yes
    DisableForwarding yes
    PermitTTY no
    PermitUserEnvironment no
    PermitUserRC no
    UseDNS no
  '';
in
{
  options.services.telchar = {
    enable = lib.mkEnableOption "Telchar Nix build gateway";

    package = lib.mkOption {
      type = lib.types.package;
      description = "Telchar package to run.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "telchar";
      description = "System user owning the Telchar daemon and ingress.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "telchar";
      description = "System group owning Telchar state.";
    };

    frontendUid = lib.mkOption {
      type = lib.types.int;
      default = 995;
      description = "UID authorized to connect to the private frontend socket.";
    };

    socketPath = lib.mkOption {
      type = lib.types.str;
      default = "/run/telchar/daemon.sock";
      description = "Private daemon frontend socket.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      description = "Strict Telchar TOML configuration.";
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      description = "Additional operator-controlled daemon environment.";
    };

    credentials = lib.mkOption {
      type = lib.types.listOf (
        lib.types.submodule {
          options = {
            name = lib.mkOption {
              type = lib.types.strMatching "[A-Za-z0-9_.-]+";
              description = "Credential name exposed below CREDENTIALS_DIRECTORY.";
            };
            source = lib.mkOption {
              type = lib.types.str;
              description = "Absolute protected credential source file outside the Nix store.";
            };
          };
        }
      );
      default = [ ];
      description = "Files loaded through systemd credentials rather than the Nix store.";
    };

    backendPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "Operator-selected backend helper packages available to the daemon.";
    };

    database = {
      manage = lib.mkEnableOption "local PostgreSQL coordination";
      name = lib.mkOption {
        type = lib.types.str;
        default = "telchar";
        description = "Local PostgreSQL database name.";
      };
      url = lib.mkOption {
        type = lib.types.str;
        default = "host=/run/postgresql user=${cfg.user} dbname=${cfg.database.name}";
        defaultText = lib.literalExpression ''"host=/run/postgresql user=\${config.services.telchar.user} dbname=\${config.services.telchar.database.name}"'';
        description = "PostgreSQL connection URL used by Telchar.";
      };
      urlFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected file containing the external PostgreSQL connection URL.";
      };
      rootCertificateFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected CA certificate required by the external PostgreSQL URL.";
      };
    };

    sshHostCertificateRenewal = {
      enable = lib.mkEnableOption "atomic Telchar SSH host-certificate installation";
      candidateFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected candidate SSH host certificate installed by the renewal service.";
      };
      expectedSigningCAFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Public key of the authority trusted to sign renewed SSH host certificates.";
      };
      expectedPrincipals = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Host principals every renewed SSH host certificate must contain.";
      };
      minimumRemainingValiditySec = lib.mkOption {
        type = lib.types.ints.positive;
        default = 300;
        description = "Minimum remaining certificate validity required at installation time.";
      };
    };

    nomad = {
      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected Nomad token source mounted read-only into the Telchar service.";
      };
      renewal = {
        enable = lib.mkEnableOption "atomic Telchar Nomad token installation";
        candidateFile = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Protected candidate Nomad token installed by the renewal service.";
        };
      };
    };

    vaultAwsAuth = {
      enable = lib.mkEnableOption "Vault AWS authentication through the EC2 instance profile";
      address = lib.mkOption {
        type = lib.types.str;
        description = "Vault API address.";
      };
      role = lib.mkOption {
        type = lib.types.str;
        description = "Vault AWS authentication role.";
      };
      authMount = lib.mkOption {
        type = lib.types.str;
        default = "aws";
        description = "Vault AWS authentication mount.";
      };
      sshSignPath = lib.mkOption {
        type = lib.types.str;
        description = "Vault path that signs the existing SSH host public key.";
      };
      nomadSecretPath = lib.mkOption {
        type = lib.types.str;
        description = "Vault path returning the Nomad token.";
      };
      metadataEndpoint = lib.mkOption {
        type = lib.types.str;
        default = "http://169.254.169.254";
        description = "EC2 instance metadata endpoint.";
      };
      renewal = {
        enable = lib.mkEnableOption "scheduled Vault-backed Telchar credential renewal";
        interval = lib.mkOption {
          type = lib.types.str;
          default = "1h";
          description = "Interval between credential renewal attempts.";
        };
        randomizedDelaySec = lib.mkOption {
          type = lib.types.str;
          default = "10min";
          description = "Maximum randomized delay applied to scheduled renewals.";
        };
      };
    };

    callback = {
      enable = lib.mkEnableOption "the authenticated Nomad callback listener";
      bindAddress = lib.mkOption {
        type = lib.types.str;
        default = "0.0.0.0";
        description = "Address for the authenticated Nomad callback listener.";
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 7443;
        description = "TCP port for the authenticated Nomad callback listener.";
      };
      openFirewall = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Whether to open the callback listener port in the host firewall.";
      };
    };

    gatewayStore = {
      uri = lib.mkOption {
        type = lib.types.str;
        default = "unix:///nix/var/nix/daemon-socket/socket";
        description = "Gateway store URI used for closure and output transfer.";
      };
      gcRootDirectory = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/telchar/gc-roots";
        description = "Directory holding retained gateway-store GC roots.";
      };
      manageTrustedUser = lib.mkEnableOption "Telchar access through the host Nix trusted-users list";
      manageGcRootDirectory = lib.mkEnableOption "the gateway-store GC-root directory";
    };

    ingress.openssh = {
      enable = lib.mkEnableOption "isolated stock-Nix OpenSSH ingress";
      port = lib.mkOption {
        type = lib.types.port;
        default = 2222;
        description = "TCP port for the isolated Telchar SSH daemon.";
      };
      listenAddresses = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "0.0.0.0" ];
        description = "Addresses for the isolated Telchar SSH daemon.";
      };
      hostKeyFile = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/telchar-ssh/ssh_host_ed25519_key";
        description = "Static host key used by the isolated Telchar SSH daemon.";
      };
      authorizedKeysFile = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/telchar/.ssh/authorized_keys";
        description = "Operator-managed authorized keys file for Telchar ingress.";
      };
      hostCertificateFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "SSH host certificate presented by the isolated Telchar ingress.";
      };
      trustedUserCAKeysFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Trusted client SSH CA public keys for the isolated Telchar ingress.";
      };
      authorizedPrincipalsFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Allowed SSH certificate principals for the Telchar account.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.hasPrefix "/run/" cfg.socketPath;
        message = "services.telchar.socketPath must be below /run";
      }
      {
        assertion =
          cfg.ingress.openssh.enable
          -> lib.all (path: lib.hasPrefix "/" path) [
            cfg.ingress.openssh.hostKeyFile
            cfg.ingress.openssh.authorizedKeysFile
          ];
        message = "services.telchar.ingress.openssh file paths must be absolute";
      }
      {
        assertion =
          !certificateIngress
          ||
            lib.all (path: path != null && lib.hasPrefix "/" path && !(lib.hasPrefix builtins.storeDir path))
              [
                cfg.ingress.openssh.hostCertificateFile
                cfg.ingress.openssh.trustedUserCAKeysFile
                cfg.ingress.openssh.authorizedPrincipalsFile
              ];
        message = "services.telchar.ingress.openssh certificate files must be absolute and outside the Nix store";
      }
      {
        assertion = lib.all (
          credential:
          lib.hasPrefix "/" credential.source && !(lib.hasPrefix builtins.storeDir credential.source)
        ) cfg.credentials;
        message = "services.telchar.credentials sources must be absolute and outside the Nix store";
      }
      {
        assertion =
          !protectedDatabase
          || (
            lib.hasPrefix "/" cfg.database.urlFile
            && !(lib.hasPrefix builtins.storeDir cfg.database.urlFile)
            && cfg.database.rootCertificateFile != null
            && lib.hasPrefix "/" cfg.database.rootCertificateFile
            && !(lib.hasPrefix builtins.storeDir cfg.database.rootCertificateFile)
          );
        message = "services.telchar.database protected files must be absolute and outside the Nix store";
      }
      {
        assertion =
          !sshRenewalCfg.enable
          || (
            certificateIngress
            && sshRenewalCfg.candidateFile != null
            && sshRenewalCfg.expectedSigningCAFile != null
            && sshRenewalCfg.expectedPrincipals != [ ]
            && lib.hasPrefix "/" sshRenewalCfg.candidateFile
            && lib.hasPrefix "/" sshRenewalCfg.expectedSigningCAFile
            && !(lib.hasPrefix builtins.storeDir sshRenewalCfg.candidateFile)
            && !(lib.hasPrefix builtins.storeDir sshRenewalCfg.expectedSigningCAFile)
          );
        message = "services.telchar.sshHostCertificateRenewal requires certificate ingress, expected signer and principals, and absolute candidate and signer paths outside the Nix store";
      }
      {
        assertion =
          !nomadRenewalCfg.enable
          || (
            cfg.nomad.tokenFile != null
            && nomadRenewalCfg.candidateFile != null
            && lib.hasPrefix "/" nomadRenewalCfg.candidateFile
            && !(lib.hasPrefix builtins.storeDir nomadRenewalCfg.candidateFile)
            && dirOf nomadRenewalCfg.candidateFile == dirOf cfg.nomad.tokenFile
          );
        message = "services.telchar.nomad.renewal requires protected token and candidate paths in the same directory outside the Nix store";
      }
      {
        assertion =
          !vaultCfg.enable
          || (
            sshRenewalCfg.enable
            && nomadRenewalCfg.enable
            && sshRenewalCfg.candidateFile != null
            && nomadRenewalCfg.candidateFile != null
            && lib.hasPrefix "/" sshRenewalCfg.candidateFile
            && lib.hasPrefix "/" nomadRenewalCfg.candidateFile
            && !(lib.hasPrefix builtins.storeDir sshRenewalCfg.candidateFile)
            && !(lib.hasPrefix builtins.storeDir nomadRenewalCfg.candidateFile)
          );
        message = "services.telchar.vaultAwsAuth requires enabled SSH and Nomad renewal with protected candidate paths";
      }
      {
        assertion = !vaultCfg.renewal.enable || vaultCfg.enable;
        message = "services.telchar.vaultAwsAuth.renewal requires Vault AWS authentication";
      }
      {
        assertion =
          cfg.nomad.tokenFile == null
          || (
            lib.hasPrefix "/" cfg.nomad.tokenFile && !(lib.hasPrefix builtins.storeDir cfg.nomad.tokenFile)
          );
        message = "services.telchar.nomad.tokenFile must be absolute and outside the Nix store";
      }
    ];

    users.groups.${cfg.group} = { };
    users.users.${cfg.user} = {
      isSystemUser = true;
      uid = cfg.frontendUid;
      group = cfg.group;
      home = "/var/lib/telchar";
      createHome = true;
      shell = "${pkgs.bashInteractive}/bin/bash";
    };

    environment.systemPackages = [ cfg.package ] ++ cfg.backendPackages;

    services.postgresql = lib.mkIf cfg.database.manage {
      enable = true;
      ensureDatabases = [ cfg.database.name ];
      ensureUsers = [
        {
          name = cfg.user;
          ensureDBOwnership = true;
        }
      ];
    };

    nix.settings.trusted-users = lib.mkIf cfg.gatewayStore.manageTrustedUser [ cfg.user ];

    environment.etc."telchar/sshd_config" = lib.mkIf cfg.ingress.openssh.enable {
      source = sshdConfiguration;
      mode = "0444";
    };

    systemd.services.telchar-sshd = lib.mkIf cfg.ingress.openssh.enable {
      description = "Telchar isolated SSH ingress";
      wantedBy = [ "multi-user.target" ];
      after = [
        "network-online.target"
        "telchar.service"
      ];
      wants = [ "network-online.target" ];
      requires = [ "telchar.service" ];
      serviceConfig = {
        RuntimeDirectory = "telchar-sshd";
        RuntimeDirectoryMode = "0755";
        ExecStart = "${pkgs.openssh}/bin/sshd -D -e -f /etc/telchar/sshd_config";
        ExecReload = "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";
      };
    };

    systemd.services.telchar-ssh-host-certificate-renewal = lib.mkIf sshRenewalCfg.enable {
      description = "Install renewed Telchar SSH host certificate";
      after = lib.optional vaultCfg.renewal.enable "telchar-vault-aws-auth.service";
      requires = lib.optional vaultCfg.renewal.enable "telchar-vault-aws-auth.service";
      serviceConfig = {
        Type = "oneshot";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = renewHostCertificate;
        ExecStartPost = "+${reloadSshIngress}";
      };
    };

    systemd.services.telchar-nomad-credential-renewal = lib.mkIf nomadRenewalCfg.enable {
      description = "Install renewed Telchar Nomad token";
      after = lib.optional vaultCfg.renewal.enable "telchar-vault-aws-auth.service";
      requires = lib.optional vaultCfg.renewal.enable "telchar-vault-aws-auth.service";
      serviceConfig = {
        Type = "oneshot";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = renewNomadToken;
        ExecStartPost = "+${reloadTelchar}";
      };
    };

    systemd.services.telchar-vault-aws-auth = lib.mkIf vaultCfg.enable {
      description = "Fetch Telchar renewal candidates through Vault AWS authentication";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = "${vaultPython}/bin/python ${fetchVaultCandidates}";
        TimeoutStartSec = 60;
        UMask = "0077";
      };
    };

    systemd.services.telchar-credential-renewal = lib.mkIf vaultCfg.renewal.enable {
      description = "Renew Telchar credentials through Vault";
      after = [
        "telchar-ssh-host-certificate-renewal.service"
        "telchar-nomad-credential-renewal.service"
      ];
      requires = [
        "telchar-ssh-host-certificate-renewal.service"
        "telchar-nomad-credential-renewal.service"
      ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/true";
      };
    };

    systemd.timers.telchar-credential-renewal = lib.mkIf vaultCfg.renewal.enable {
      description = "Schedule Telchar credential renewal";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "5min";
        OnUnitInactiveSec = vaultCfg.renewal.interval;
        RandomizedDelaySec = vaultCfg.renewal.randomizedDelaySec;
        Persistent = true;
        Unit = "telchar-credential-renewal.service";
      };
    };

    systemd.services.telchar = {
      description = "Telchar Nix build gateway";
      wantedBy = [ "multi-user.target" ];
      after = [
        "network-online.target"
      ]
      ++ lib.optionals cfg.database.manage [
        "postgresql.service"
        "postgresql-setup.service"
      ];
      wants = [ "network-online.target" ];
      requires = lib.optionals cfg.database.manage [
        "postgresql.service"
        "postgresql-setup.service"
      ];
      environment = {
        TELCHAR_CONFIG = configurationFile;
        TELCHAR_GATEWAY_STORE_URI = cfg.gatewayStore.uri;
        TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = cfg.gatewayStore.gcRootDirectory;
        TMPDIR = "/var/lib/telchar/import";
      }
      // lib.optionalAttrs (!protectedDatabase) { TELCHAR_DATABASE_URL = cfg.database.url; }
      // daemonEnvironment;
      path = [
        pkgs.nix
        pkgs.openssh
      ]
      ++ cfg.backendPackages;
      serviceConfig = {
        User = cfg.user;
        Group = cfg.group;
        RuntimeDirectory = "telchar";
        RuntimeDirectoryMode = "0700";
        StateDirectory = "telchar";
        StateDirectoryMode = "0700";
        ExecStart = "${cfg.package}/bin/telchar daemon --socket ${cfg.socketPath} --frontend-uid ${toString cfg.frontendUid}";
        ExecStartPre = lib.optional protectedDatabase databaseValidator;
        Restart = "on-failure";
        LoadCredential = credentialFiles;
        BindReadOnlyPaths = lib.optional (
          cfg.nomad.tokenFile != null
        ) "${dirOf cfg.nomad.tokenFile}:/run/telchar/credentials";
      };
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/telchar/import 0700 ${cfg.user} ${cfg.group} -"
    ]
    ++ lib.optional cfg.gatewayStore.manageGcRootDirectory "d ${cfg.gatewayStore.gcRootDirectory} 0700 ${cfg.user} ${cfg.group} -";
  };
}
