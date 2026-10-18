# Image-owned sandbox policy and dedicated build identities.
{ pkgs }:
pkgs.runCommand "microvm-store-etc" { } ''
  mkdir -p "$out/etc/nix"
  echo 'root:x:0:0:root:/root:/bin/sh' > "$out/etc/passwd"
  echo 'root:x:0:' > "$out/etc/group"
  members=""
  for n in $(seq 1 16); do
    echo "nixbld$n:x:$((30000+n)):30000:Nix build user:/var/empty:/bin/sh" >> "$out/etc/passwd"
    members="$members''${members:+,}nixbld$n"
  done
  echo "nixbld:x:30000:$members" >> "$out/etc/group"
  cat > "$out/etc/nix/nix.conf" <<'EOF'
  experimental-features = nix-command
  sandbox = true
  sandbox-fallback = false
  build-users-group = nixbld
  max-jobs = 1
  cores = 1
  substituters =
  EOF
''
