#!/usr/bin/env bash
# Disposable, explicitly privileged local sandbox test. Never mounts host /nix.
set -euo pipefail
cd "$(dirname "$0")/../.."
image=$(nix build --impure --no-link --print-out-paths --expr '
  let f = builtins.getFlake (toString ./.);
      pkgs = import f.inputs.nixpkgs { system = builtins.currentSystem; };
  in import ./nix/microvm-probe.nix { inherit pkgs; }')
docker load -i "$image"
container=$(docker run --detach --privileged --network none telchar-microvm-probe:experimental)
trap 'docker rm -f "$container" >/dev/null' EXIT
# -i keeps stdin open for the behavioral test script.
docker exec -i "$container" /bin/python3 - <<'PY'
import json
import time
import urllib.error
import urllib.request

url = 'http://127.0.0.1:8080/aws/lambda-microvms/runtime/v1/'
def post(hook, body=b'{}'):
    request = urllib.request.Request(url + hook, data=body)
    with urllib.request.urlopen(request, timeout=2) as response:
        return json.load(response)

deadline = time.monotonic() + 60
while True:
    try:
        assert post('ready')['status'] == 'idle'
        break
    except (OSError, urllib.error.URLError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(.1)
post('run', b'{"microvmId":"local-sandbox-test"}')
while time.monotonic() < deadline:
    try:
        result = post('ready')['status']
        if result == 'failed':
            raise RuntimeError('sandbox probe failed')
        if result == 'succeeded':
            print('sandbox probe passed')
            break
    except urllib.error.HTTPError as error:
        if error.code != 409:
            raise
    time.sleep(.1)
else:
    raise TimeoutError('sandbox probe did not finish')
PY
logs=$(docker logs "$container" 2>&1)
printf '%s\n' "$logs"
if printf '%s\n' "$logs" | grep -Ei 'error:|warning:'; then
  echo 'sandbox qualification emitted errors or warnings' >&2
  exit 1
fi
