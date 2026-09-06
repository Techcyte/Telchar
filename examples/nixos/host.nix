# Configures gateway host infrastructure and credential installation.
{
  config,
  lib,
  pkgs,
  telcharSshCommand,
  ...
}:
let
  cfg = config.services.telchar;
  sshRenewalCfg = cfg.sshHostCertificateRenewal;
  nomadRenewalCfg = cfg.nomad.renewal;
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
  forcedCommand = telcharSshCommand {
    inherit pkgs lib;
    inherit (cfg) package socketPath;
  };
  certificateIngress = cfg.ingress.openssh.hostCertificateFile != null;
  sshdConfiguration = pkgs.writeText "telchar-sshd_config" ''
    Port ${toString cfg.ingress.openssh.port}
    ${lib.concatMapStringsSep "\n" (
      address: "ListenAddress ${address}"
    ) cfg.ingress.openssh.listenAddresses}
    HostKey ${cfg.ingress.openssh.hostKeyFile}
    ${lib.optionalString certificateIngress "HostCertificate ${cfg.ingress.openssh.hostCertificateFile}"}
    ${lib.optionalString (
      cfg.ingress.openssh.trustedUserCAKeysFile != null
    ) "TrustedUserCAKeys ${cfg.ingress.openssh.trustedUserCAKeysFile}"}
    ${lib.optionalString (
      cfg.ingress.openssh.authorizedPrincipalsFile != null
    ) "AuthorizedPrincipalsFile ${cfg.ingress.openssh.authorizedPrincipalsFile}"}
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
      renewal = {
        enable = lib.mkEnableOption "atomic Telchar Nomad token installation";
        candidateFile = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Protected candidate Nomad token installed by the renewal service.";
        };
      };
    };

    database = {
      manage = lib.mkEnableOption "local PostgreSQL coordination";
      name = lib.mkOption {
        type = lib.types.str;
        default = "telchar";
        description = "Local PostgreSQL database name.";
      };
    };
    gatewayStore = {
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
          lib.all (path: path == null || (lib.hasPrefix "/" path && !(lib.hasPrefix builtins.storeDir path)))
            [
              cfg.ingress.openssh.hostCertificateFile
              cfg.ingress.openssh.trustedUserCAKeysFile
              cfg.ingress.openssh.authorizedPrincipalsFile
            ];
        message = "services.telchar.ingress.openssh certificate files must be absolute and outside the Nix store";
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
    ];
    services.telchar.database.url = lib.mkIf cfg.database.manage (
      lib.mkDefault "host=/run/postgresql user=${cfg.user} dbname=${cfg.database.name}"
    );
    users.users.${cfg.user}.shell = "${pkgs.bashInteractive}/bin/bash";
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
      unitConfig.Requisite = "telchar.service";
      serviceConfig = {
        RuntimeDirectory = "telchar-sshd";
        RuntimeDirectoryMode = "0755";
        ExecStart = "${pkgs.openssh}/bin/sshd -D -e -f /etc/telchar/sshd_config";
        ExecReload = "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";
        RestartSec = "5s";
      };
    };

    systemd.services.telchar-ssh-host-certificate-renewal = lib.mkIf sshRenewalCfg.enable {
      description = "Install renewed Telchar SSH host certificate";
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
      serviceConfig = {
        Type = "oneshot";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = renewNomadToken;
        ExecStartPost = "+${reloadTelchar}";
      };
    };

    systemd.services.telchar = {
      unitConfig.Upholds = lib.optional cfg.ingress.openssh.enable "telchar-sshd.service";
      after = lib.optionals cfg.database.manage [
        "postgresql.service"
        "postgresql-setup.service"
      ];
      requires = lib.optionals cfg.database.manage [
        "postgresql.service"
        "postgresql-setup.service"
      ];
    };
    systemd.tmpfiles.rules = lib.optional cfg.gatewayStore.manageGcRootDirectory "d ${cfg.gatewayStore.gcRootDirectory} 0700 ${cfg.user} ${cfg.group} -";
  };
}
