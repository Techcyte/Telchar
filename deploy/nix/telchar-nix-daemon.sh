#!/bootstrap/busybox sh
set -eu

if [ ! -x /bin/nix ]; then
	/bootstrap/busybox tar -xf /bootstrap/store.tar -C /
fi

/bootstrap/busybox mkdir -p /nix/store /nix/var/nix/daemon-socket /nix/var/nix/db /nix/var/log/nix/drvs
if [ ! -f /nix/var/nix/db/db.sqlite ]; then
	/bin/nix-store --load-db </bootstrap/registration
fi
/bootstrap/busybox rm -f /nix/var/nix/daemon-socket/socket

exec /bin/nix daemon --force-trusted
