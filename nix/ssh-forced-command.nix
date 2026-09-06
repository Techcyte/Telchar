# Builds the OpenSSH-authenticated Telchar stdio entry point.
{
  pkgs,
  lib,
  package,
  socketPath,
}:
pkgs.writeShellScript "telchar-forced-command" ''
  export PATH=${
    lib.makeBinPath [
      pkgs.coreutils
      pkgs.gawk
      pkgs.openssh
    ]
  }
  export TELCHAR_IPC_SOCKET=${lib.escapeShellArg socketPath}
  export TELCHAR_PROGRAM=${lib.escapeShellArg "${package}/bin/telchar"}
  ${builtins.readFile ../deploy/ssh/telchar-ssh-forced-command.sh}
''
