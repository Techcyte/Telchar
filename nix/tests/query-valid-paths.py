"""Checks daemon and SSH validity operations against real store registrations."""

import json
import socket
import struct
import subprocess
import sys


def integer(source):
    data = source.read(8)
    assert len(data) == 8, data
    return struct.unpack("<Q", data)[0]


def string(source):
    size = integer(source)
    assert size <= 65536, size
    value = source.read(size)
    assert len(value) == size
    padding = source.read((-size) % 8)
    assert padding == b"\0" * ((-size) % 8)
    return value


def encoded(value):
    return struct.pack("<Q", len(value)) + value + b"\0" * ((-len(value)) % 8)


def fields(source):
    count = integer(source)
    assert count <= 64
    for _ in range(count):
        kind = integer(source)
        if kind == 0:
            integer(source)
        else:
            assert kind == 1
            string(source)


def finish(source):
    while True:
        marker = integer(source)
        if marker == 0x616C7473:
            return
        if marker == 0x6F6C6D67:
            string(source)
        elif marker == 0x53545254:
            for _ in range(3):
                integer(source)
            string(source)
            fields(source)
            integer(source)
        elif marker == 0x53544F50:
            integer(source)
        elif marker == 0x52534C54:
            integer(source)
            integer(source)
            fields(source)
        else:
            raise AssertionError(f"unexpected protocol marker {marker:x}")


transport, path = sys.argv[1:]
process = None
connection = None
if transport == "ssh":
    process = subprocess.Popen(
        ["ssh", "-4", "-T", "-p", "2222", "telchar@gateway"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    )
    assert process.stdout is not None and process.stdin is not None
    source, sink = process.stdout, process.stdin
else:
    connection = socket.socket(socket.AF_UNIX)
    connection.connect(transport)
    source = sink = connection.makefile("rwb", buffering=0)

sink.write(struct.pack("<QQQQQ", 0x6E697863, 0x126, 0, 0, 0))
sink.flush()
assert integer(source) == 0x6478696F
assert integer(source) >= 0x123
assert integer(source) == 0
string(source)
assert integer(source) in [0, 1, 2]
finish(source)
paths = [path.encode()]
for substitute, expected in [(False, []), (True, paths), (False, paths)]:
    sink.write(struct.pack("<QQ", 31, len(paths)))
    for value in paths:
        sink.write(encoded(value))
    sink.write(struct.pack("<Q", substitute))
    sink.flush()
    finish(source)
    count = integer(source)
    assert count <= len(paths)
    actual = [string(source) for _ in range(count)]
    assert actual == expected, (substitute, actual, expected)
    print(json.dumps({"transport": transport, "substitute": substitute, "valid_count": count}))

sink.close()
if process is not None:
    source.close()
    assert process.wait(timeout=10) == 0
else:
    assert connection is not None
    connection.close()
