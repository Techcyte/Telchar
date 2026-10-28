#!/usr/bin/env python3
"""Qualify a prepared ARM runtime root against the configured gateway fixture."""
import argparse
import http.client
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def bridge(binding):
    directory = Path(os.environ['MICROVM_RUNTIME_BRIDGE'])
    temporary = directory / 'binding.tmp'
    temporary.write_bytes(binding.read_bytes())
    temporary.rename(directory / 'binding.json')
    deadline = time.monotonic() + 45
    while not (directory / 'result').exists():
        if time.monotonic() >= deadline:
            raise TimeoutError('runtime completion')
        time.sleep(.025)
    return int((directory / 'result').read_text())


def request(name, value=None):
    connection = http.client.HTTPConnection('127.0.0.1', 8080, timeout=2)
    try:
        connection.request('POST', '/aws/lambda-microvms/runtime/v1/' + name,
                           json.dumps(value or {}))
        response = connection.getresponse()
        return response.status, json.loads(response.read())
    finally:
        connection.close()


def qualify(args):
    root = args.root.resolve(strict=True)
    context = args.context.resolve(strict=True)
    evidence = Path(tempfile.mkdtemp(prefix='runtime-exchange-', dir=args.evidence))
    print('EVIDENCE=' + str(evidence), flush=True)
    print('FREE_BYTES=' + str(shutil.disk_usage(root).free), flush=True)
    assert (context / 'runtime/nix.conf').read_bytes() == (root / 'etc/nix/nix.conf').read_bytes()
    chroot = shutil.which('chroot')
    assert chroot

    def mount(*arguments):
        subprocess.run(['mount', *map(str, arguments)], check=True)

    mount('--make-rprivate', '/')
    mount('--bind', root, root)
    mount('--bind', context / 'runtime', root / 'opt/telchar')
    # Independent store and database prevent successful outputs from being reused.
    mount('-t', 'tmpfs', '-o', 'mode=0755', 'tmpfs', root / 'nix/var')
    store = evidence / 'store'
    for name in ['upper', 'work', 'merged']:
        (store / name).mkdir(parents=True)
    mount('-t', 'overlay', 'overlay', '-o',
          f'lowerdir={context / "nix/store"},upperdir={store / "upper"},workdir={store / "work"}',
          store / 'merged')
    mount('--bind', store / 'merged', root / 'nix/store')
    for name in ['null', 'zero', 'full', 'random', 'urandom', 'tty']:
        mount('--bind', '/dev/' + name, root / 'dev' / name)
    mount('-t', 'devpts', '-o', 'newinstance,ptmxmode=0666,mode=0620',
          'devpts', root / 'dev/pts')
    assert (root / 'dev/ptmx').is_symlink()
    mount('-t', 'proc', 'proc', root / 'proc')
    mount('-t', 'tmpfs', '-o', 'mode=0755', 'tmpfs', root / 'run')
    mount('-t', 'tmpfs', '-o', 'mode=1777', 'tmpfs', root / 'tmp')
    directory = evidence / 'bridge'
    directory.mkdir()
    gateway_env = os.environ.copy()
    gateway_env.update({
        'MICROVM_RUNTIME_BRIDGE': str(directory),
        'MICROVM_TEST_RUNTIME': str(Path(__file__).resolve()),
        'MICROVM_TEST_BUILDER': args.builder,
        'MICROVM_TEST_REGISTRATION': str(context / 'runtime/registration'),
    })
    if args.compiler:
        gateway_env['MICROVM_TEST_COMPILER'] = args.compiler
    runtime_env = {'HOME': '/root', 'PATH': '/usr/sbin:/usr/bin:/sbin:/bin',
                   'PYTHONDONTWRITEBYTECODE': '1'}
    with (evidence / 'stdout').open('wb') as stdout, (evidence / 'stderr').open('wb') as stderr, (evidence / 'gateway.stdout').open('wb') as gateway_stdout, (evidence / 'gateway.stderr').open('wb') as gateway_stderr:
        child = subprocess.Popen([chroot, str(root), '/bin/sh', '/opt/telchar/entrypoint'],
                                 env=runtime_env, stdout=stdout, stderr=stderr)
        gateway = None
        try:
            deadline = time.monotonic() + 15
            while True:
                assert child.poll() is None, 'entrypoint exited; retained logs'
                try:
                    assert request('ready') == (200, {'status': 'idle'})
                    break
                except ConnectionRefusedError:
                    assert time.monotonic() < deadline, 'readiness timeout'
                    time.sleep(.05)
            gateway = subprocess.Popen(
                ['unshare', '--user', '--map-user=1000', '--map-group=100', *args.gateway],
                env=gateway_env, stdout=gateway_stdout, stderr=gateway_stderr)
            deadline = time.monotonic() + 30
            while not (directory / 'binding.json').exists():
                assert gateway.poll() is None, 'gateway exited before binding; retained logs'
                assert time.monotonic() < deadline, 'gateway binding timeout'
                time.sleep(.025)
            payload = json.loads((directory / 'binding.json').read_text())
            provider_id = payload.pop('provider_id')
            payload['version'] = 1
            hook = {'microvmId': provider_id, 'runHookPayload': json.dumps(payload)}
            assert request('run', hook) == (200, {'status': 'running'})
            deadline = time.monotonic() + 30
            while True:
                code, state = request('ready')
                assert code == 409, (code, state)
                if state['status'] != 'running':
                    assert state['status'] == 'succeeded', state
                    break
                assert gateway.poll() is None, 'gateway exited during execution; retained logs'
                assert time.monotonic() < deadline, 'worker execution timeout'
                time.sleep(.025)
            (directory / 'result').write_text('0')
            assert gateway.wait(timeout=15) == 0, 'gateway failed; retained logs'
            assert (evidence / 'gateway.stderr').read_bytes() == b''
            print('NATIVE_WORKER_GATEWAY_REGISTERED_OUTPUT=verified', flush=True)
        finally:
            if not (directory / 'result').exists():
                (directory / 'result').write_text('1')
            if gateway is not None:
                try:
                    gateway.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    gateway.terminate()
                    gateway.wait(timeout=5)
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        assert child.returncode == 0
    assert (evidence / 'stdout').read_bytes() == b''
    assert (evidence / 'stderr').read_bytes() == b''
    print('NATIVE_EXCHANGE_SHUTDOWN=verified', flush=True)
    return 0


def main():
    if len(sys.argv) == 2 and 'MICROVM_RUNTIME_BRIDGE' in os.environ:
        return bridge(Path(sys.argv[1]))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--context', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--builder', required=True)
    parser.add_argument('--compiler', help='declared native GCC executable for compilation workload')
    parser.add_argument('gateway', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.gateway:
        parser.error('gateway fixture command is required')
    return qualify(args)


if __name__ == '__main__':
    sys.exit(main())
