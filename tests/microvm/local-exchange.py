#!/usr/bin/env python3
"""Execute real worker WSS input/build/output exchange in disposable stores."""
import json
import secrets
import subprocess
import tempfile
import time
from pathlib import Path


def run(*arguments, **options):
    result = subprocess.run(arguments, capture_output=True, **options)
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} failed: {result.stderr.decode()}")
    return result.stdout


def copy_binary(container, binary):
    libraries = []
    for line in run("ldd", str(binary)).decode().splitlines():
        libraries.extend(word for word in line.split() if word.startswith("/nix/store/"))
    libraries.append(run("patchelf", "--print-interpreter", str(binary)).decode().strip())
    for library in set(libraries):
        run("docker", "exec", container, "/bin/mkdir", "-p", str(Path(library).parent))
        run("docker", "cp", "-L", library, f"{container}:{library}")
    run("docker", "cp", str(binary), f"{container}:/run/{binary.name}")


PREPARE = r'''
import json, pathlib, subprocess

def command(*args):
    result = subprocess.run(args, capture_output=True)
    assert result.returncode == 0, result.stderr.decode()
    assert not result.stderr, result.stderr.decode()
    return result.stdout.decode().strip()

pathlib.Path('/tmp/input').write_bytes(b'closure transport payload\n' * 40000)
source = command('/bin/nix-store', '--add', '/tmp/input')
shell = pathlib.Path('/bin/bash').resolve()
package = shell.parent.parent
expression = 'builtins.derivation { name = "wss-distinct-store-proof"; system = "x86_64-linux"; shell = builtins.storePath "%s"; source = builtins.storePath "%s"; builder = "%s"; args = [ "-euc" "test ! -e /run/telchar-microvm-host-only; test \\"$EUID\\" != 0; while IFS= read -r line; do printf \\"%%s\\\\n\\" \\"$line\\"; done < $source > $out" ]; }' % (package, source, shell)
# JSON quoting keeps Nix and shell argument boundaries exact.
script = 'test ! -e /run/telchar-microvm-host-only; test "$EUID" != 0; while IFS= read -r line; do printf "%s\\n" "$line"; done < "$source" > "$out"'
expression = 'builtins.derivation { name = "wss-distinct-store-proof"; system = "x86_64-linux"; shell = builtins.storePath %s; source = builtins.storePath %s; builder = %s; args = [ "-euc" %s ]; }' % tuple(map(json.dumps, [str(package), source, str(shell), script]))
root = command('/bin/nix-instantiate', '--expr', expression, '--add-root', '/run/exchange-derivation')
derivation = str(pathlib.Path(root).resolve())
shown = json.loads(command('/bin/nix', '--offline', 'derivation', 'show', derivation))
drv = next(iter(shown['derivations'].values()))
b = lambda value: list(value.encode())
output = '/nix/store/' + drv['outputs']['out']['path']
closure = command('/bin/nix-store', '--query', '--requisites', source, str(package)).splitlines()
paths = []
for index, path in enumerate(closure):
    info = json.loads(command('/bin/nix', '--offline', 'path-info', '--json', '--json-format', '1', path))[path]
    import base64
    digest = base64.b64decode(info['narHash'].split('-', 1)[1]).hex()
    paths.append(dict(path=path, nar_hash=digest, nar_size=info['narSize'], references=info['references'], deriver=None, content_address=info.get('ca')))
    with open('/run/%d.nar' % index, 'wb') as archive:
        dumped = subprocess.run(['/bin/nix-store', '--dump', path], stdout=archive, stderr=subprocess.PIPE)
        assert dumped.returncode == 0 and not dumped.stderr, dumped.stderr.decode()
build = dict(derivation_path=b(derivation), outputs=[dict(name=b('out'), path=b(output), hash_algorithm=[], hash=[])], input_sources=[b('/nix/store/' + path) for path in drv['inputs']['srcs']], system=drv['system'], required_system_features=[], builder=b(drv['builder']), arguments=list(map(b, drv['args'])), environment=[[b(name), b(value)] for name,value in drv['env'].items()])
manifest = dict(derivation_path=derivation, build=build, paths=paths, outputs=[output])
pathlib.Path('/run/manifest.json').write_text(json.dumps(manifest))
print(json.dumps(dict(source=source, output=output, manifest=manifest)))
'''


def main():
    containers = []
    processes = []
    network = None
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="telchar-wss-") as directory:
        root = Path(directory)
        try:
            network = run("docker", "network", "create", "--internal", "telchar-wss-" + secrets.token_hex(6)).decode().strip()
            for alias in ["receiver", "worker"]:
                container = run("docker", "run", "--detach", "--privileged", "--network", network,
                                "--network-alias", alias, "telchar-microvm-probe:experimental").decode().strip()
                containers.append(container)
                deadline = time.monotonic() + 30
                while subprocess.run(["docker", "exec", container, "/bin/nix", "--offline", "store", "ping", "--store", "daemon"], capture_output=True).returncode:
                    if time.monotonic() > deadline:
                        raise TimeoutError("daemon readiness")
                    time.sleep(.1)
            gateway, worker = containers
            for container, name in [(gateway, "microvm-receiver"), (worker, "microvm-worker")]:
                copy_binary(container, Path("target/debug/examples") / name)
            prepared = json.loads(run("docker", "exec", "-i", gateway, "/bin/python3", "-", input=PREPARE.encode()))
            # A transferred source cannot be preloaded in the worker image.
            run("docker", "exec", worker, "/bin/bash", "-ec", 'test ! -e "$1"', "check", prepared["source"])
            run("docker", "exec", gateway, "/bin/bash", "-ec", 'test ! -e "$1"', "check", prepared["output"])
            run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(root / "key.pem"),
                "-out", str(root / "certificate.pem"), "-days", "1", "-subj", "/CN=receiver",
                "-addext", "subjectAltName=DNS:receiver", "-addext", "basicConstraints=critical,CA:FALSE")
            credential = secrets.token_hex(32)
            config = {"execution": "local-wss", "microvmId": "local-worker", "credential": credential,
                          "manifest": prepared["manifest"], "directory": "/run", "store_uri": "unix:///nix/var/nix/daemon-socket/socket"}
            (root / "receiver.json").write_text(json.dumps(config))
            certificate = ssl_der(root / "certificate.pem")
            (root / "worker.json").write_text(json.dumps({"endpoint": "wss://receiver:8443/fixture", "credential": credential,
                "certificate": certificate, "execution": "local-wss", "microvmId": "local-worker"}))
            for name in ["receiver.json", "certificate.pem", "key.pem"]:
                run("docker", "cp", str(root / name), f"{gateway}:/run/{name}")
            run("docker", "cp", str(root / "worker.json"), f"{worker}:/run/worker.json")
            receiver = subprocess.Popen(["docker", "exec", gateway, "/run/microvm-receiver", "/run/receiver.json",
                                         "/run/certificate.pem", "/run/key.pem", "0.0.0.0:8443"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            processes.append(receiver)
            time.sleep(.3)
            result = run("docker", "exec", worker, "/run/microvm-worker", "/run/worker.json", timeout=180)
            stdout, stderr = receiver.communicate(timeout=30)
            assert receiver.returncode == 0 and not stderr, stderr.decode()
            assert b"microvm outbound build transfer succeeded" in result, result.decode()
            run("docker", "exec", gateway, "/bin/bash", "-ec", 'test -e "$1"', "check", prepared["output"])
            source = run("docker", "exec", gateway, "/bin/cat", prepared["source"])
            output = run("docker", "exec", gateway, "/bin/cat", prepared["output"])
            assert output == source, (len(output), len(source))
            for container in containers:
                captured = subprocess.run(["docker", "logs", container], capture_output=True, check=True)
                logs = (captured.stdout + captured.stderr).decode()
                assert "error:" not in logs.lower() and "warning:" not in logs.lower(), logs
            print(json.dumps({"outcome": "succeeded", "source_bytes": len(source), "output_bytes": len(output), "elapsed_seconds": time.monotonic()-started}))
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.communicate()
            for container in reversed(containers):
                run("docker", "rm", "-f", container)
            if network:
                run("docker", "network", "rm", network)


def ssl_der(path):
    import ssl
    return ssl.PEM_cert_to_DER_cert(path.read_text()).hex()


if __name__ == "__main__":
    main()
