#!/bootstrap/busybox sh
set -eu

if [ ! -x @nix@/bin/nix ]; then
	/bootstrap/busybox tar -oxf /bootstrap/store.tar -C /
fi

/bootstrap/busybox mkdir -p /nix/store /nix/var/nix/daemon-socket /nix/var/nix/db /nix/var/log/nix/drvs
if [ ! -f /nix/var/nix/db/db.sqlite ]; then
	@nix@/bin/nix-store --load-db </bootstrap/registration
fi
/bootstrap/busybox rm -f /nix/var/nix/daemon-socket/socket

exec @nix@/bin/nix --extra-experimental-features 'nix-command daemon-trust-override' daemon --force-trusted
