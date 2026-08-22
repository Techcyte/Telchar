#!/bootstrap/busybox sh
set -eu

if [ ! -x @nix@/bin/nix ] || [ ! -r @cacert@/etc/ssl/certs/ca-bundle.crt ]; then
	/bootstrap/busybox tar -oxf /bootstrap/store.tar -C /
fi

/bootstrap/busybox mkdir -p /nix/store /nix/var/nix/daemon-socket /nix/var/nix/db /nix/var/log/nix/drvs
@nix@/bin/nix-store --load-db </bootstrap/registration
/bootstrap/busybox rm -f /nix/var/nix/daemon-socket/socket

exec @nix@/bin/nix --extra-experimental-features 'nix-command daemon-trust-override' daemon --force-trusted
