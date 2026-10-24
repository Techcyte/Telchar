"""Sample gateway staging, including unnamed NAR files held by the daemon."""
import json
import os
import sys
import time
from pathlib import Path

pid = int(sys.argv[1])
staging = Path('/var/lib/telchar')
with Path('/run/microvm-staging.jsonl').open('x') as output:
    while not Path('/run/microvm-staging-stop').exists():
        files = {}
        try:
            descriptors = list(Path(f'/proc/{pid}/fd').iterdir())
        except FileNotFoundError:
            raise RuntimeError('gateway exited during staging observation') from None
        for descriptor in descriptors:
            try:
                target = os.readlink(descriptor)
                if not Path(target.removesuffix(' (deleted)')).is_relative_to(staging):
                    continue
                value = descriptor.stat()
            except FileNotFoundError:
                continue
            files[(value.st_dev, value.st_ino)] = value
        output.write(json.dumps({
            'monotonic': time.monotonic(),
            'logical_bytes': sum(value.st_size for value in files.values()),
            'allocated_bytes': sum(value.st_blocks * 512 for value in files.values()),
        }) + '\n')
        output.flush()
        time.sleep(.01)
