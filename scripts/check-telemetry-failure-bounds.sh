#!/bin/sh
set -eu

output=$(mktemp)
trap 'rm -f "$output"' EXIT

python3 -c '
import socket
import time

listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 43171))
listener.listen()
connection, _ = listener.accept()
time.sleep(10)
connection.close()
listener.close()
' &
collector_pid=$!
trap 'kill "$collector_pid" 2>/dev/null || true; rm -f "$output"' EXIT

start=$(date +%s)
if ! timeout 5s env OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:43171 \
	nix develop -c cargo run -p telchar --locked >"$output" 2>&1; then
	cat "$output" >&2
	exit 1
fi
elapsed=$(($(date +%s) - start))

if [ "$elapsed" -gt 4 ]; then
	printf 'stalled collector exceeded shutdown bound: %ss\n' "$elapsed" >&2
	exit 1
fi

if ! grep -Fx 'Nix worker protocol' "$output" >/dev/null; then
	cat "$output" >&2
	exit 1
fi

if grep -Ei 'panic|stack overflow|recurs' "$output" >/dev/null; then
	cat "$output" >&2
	exit 1
fi

printf 'telemetry exporter failure bounds check passed in %ss\n' "$elapsed"
