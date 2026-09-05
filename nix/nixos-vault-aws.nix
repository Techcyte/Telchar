# Supplies optional Vault AWS credentials to independent Telchar consumers.
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
  sshIssuance = vaultCfg.sshSignPath != null;
  nomadIssuance = vaultCfg.nomadSecretPath != null;
  renewalServices =
    lib.optional sshIssuance "telchar-ssh-host-certificate-renewal.service"
    ++ lib.optional nomadIssuance "telchar-nomad-credential-renewal.service";
  fetchVaultCandidates = pkgs.writeText "fetch-telchar-vault-candidates.py" ''
    import base64
    import json
    import os
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
    sts_url = ${builtins.toJSON vaultCfg.stsEndpoint}
    sts_body = "Action=GetCallerIdentity&Version=2011-06-15"
    aws_request = AWSRequest(
        method="POST",
        url=sts_url,
        data=sts_body,
        headers={"Content-Type": "application/x-www-form-urlencoded; charset=utf-8"},
    )
    SigV4Auth(credentials, "sts", ${builtins.toJSON vaultCfg.region}).add_auth(aws_request)
    prepared_request = aws_request.prepare()
    login_payload = {
        "role": ${builtins.toJSON vaultCfg.role},
        "iam_http_request_method": "POST",
        "iam_request_url": base64.b64encode(sts_url.encode()).decode(),
        "iam_request_body": base64.b64encode(sts_body.encode()).decode(),
        "iam_request_headers": base64.b64encode(json.dumps(dict(prepared_request.headers)).encode()).decode(),
    }

    def vault_request(path, method="GET", payload=None, token=None, text=False):
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
        response = bounded_request(request)
        return response.decode() if text else json.loads(response)

    login = vault_request(
        "auth/${vaultCfg.authMount}/login",
        method="POST",
        payload=login_payload,
    )
    vault_token = login["auth"]["client_token"]
    ${lib.optionalString sshIssuance ''
      with open(${builtins.toJSON "${cfg.ingress.openssh.hostKeyFile}.pub"}) as host_public_key:
          signed_certificate = vault_request(
              ${builtins.toJSON vaultCfg.sshSignPath},
              method="POST",
              payload={
                  "public_key": host_public_key.read(),
                  "cert_type": "host",
                  "valid_principals": ${builtins.toJSON (lib.concatStringsSep "," cfg.sshHostCertificateRenewal.expectedPrincipals)},
              },
              token=vault_token,
          )["data"]["signed_key"]
    ''}
    ${lib.optionalString nomadIssuance ''
      nomad_secret = vault_request(
          ${builtins.toJSON vaultCfg.nomadSecretPath},
          token=vault_token,
      )["data"]
      nomad_token = nomad_secret.get("secret_id")
      if nomad_token is None:
          nomad_token = nomad_secret["data"]["token"]
    ''}
    ${lib.optionalString (vaultCfg.hostCAPath != null) ''
      host_ca = vault_request(
          ${builtins.toJSON vaultCfg.hostCAPath},
          token=vault_token,
          text=True,
      )
    ''}
    ${lib.optionalString (vaultCfg.clientCAPath != null) ''
      client_ca = vault_request(
          ${builtins.toJSON vaultCfg.clientCAPath},
          token=vault_token,
          text=True,
      )

    ''}

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

    ${lib.optionalString sshIssuance "write_candidate(${builtins.toJSON sshRenewalCfg.candidateFile}, signed_certificate)"}
    ${lib.optionalString nomadIssuance "write_candidate(${builtins.toJSON nomadRenewalCfg.candidateFile}, nomad_token)"}
    ${lib.optionalString (
      vaultCfg.hostCAPath != null
    ) "write_candidate(${builtins.toJSON sshRenewalCfg.expectedSigningCAFile}, host_ca)"}
    ${lib.optionalString (
      vaultCfg.clientCAPath != null
    ) "write_candidate(${builtins.toJSON cfg.ingress.openssh.trustedUserCAKeysFile}, client_ca)"}
  '';
in
{
  options.services.telchar = {
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
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Vault path that signs the existing SSH host public key.";
      };
      nomadSecretPath = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Vault path returning the Nomad token.";
      };
      hostCAPath = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional Vault API path returning the host CA public key.";
      };
      clientCAPath = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional Vault API path returning the client CA public key.";
      };
      region = lib.mkOption {
        type = lib.types.str;
        default = "us-east-1";
        description = "AWS STS signing region.";
      };
      stsEndpoint = lib.mkOption {
        type = lib.types.str;
        default = "https://sts.amazonaws.com/";
        description = "AWS STS endpoint signed for Vault authentication.";
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

  };
  config = lib.mkIf (cfg.enable && vaultCfg.enable) {
    assertions = [
      {
        assertion = !sshIssuance || sshRenewalCfg.enable;
        message = "Vault SSH issuance requires SSH host certificate renewal";
      }
      {
        assertion = !nomadIssuance || nomadRenewalCfg.enable;
        message = "Vault Nomad issuance requires Nomad credential renewal";
      }
      {
        assertion = vaultCfg.hostCAPath == null || sshRenewalCfg.expectedSigningCAFile != null;
        message = "Vault host CA delivery requires expectedSigningCAFile";
      }
      {
        assertion = vaultCfg.clientCAPath == null || cfg.ingress.openssh.trustedUserCAKeysFile != null;
        message = "Vault client CA delivery requires trustedUserCAKeysFile";
      }
    ];
    systemd.services.telchar-ssh-host-certificate-renewal = lib.mkIf sshIssuance {
      after = [ "telchar-vault-aws-auth.service" ];
      requires = [ "telchar-vault-aws-auth.service" ];
    };
    systemd.services.telchar-nomad-credential-renewal = lib.mkIf nomadIssuance {
      after = [ "telchar-vault-aws-auth.service" ];
      requires = [ "telchar-vault-aws-auth.service" ];
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
      after = [ "telchar-vault-aws-auth.service" ] ++ renewalServices;
      requires = [ "telchar-vault-aws-auth.service" ] ++ renewalServices;
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

  };
}
