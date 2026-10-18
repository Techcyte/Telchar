#!/usr/bin/env python3
"""Build isolated matching AWS-patched OpenSSL; not snapshot or image qualification."""

import argparse
import ctypes
import hashlib
import pathlib
import platform
import subprocess
import tarfile
import tempfile


def verified_file(path, expected, limit):
    if path.stat().st_size > limit:
        raise ValueError(f"artifact exceeds size limit: {path}")
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise ValueError(f"artifact checksum mismatch: {path}")
    return path.resolve()


def probe(tree):
    library = ctypes.CDLL(str(tree / "libcrypto.so.3"))
    random_bytes = library.RAND_bytes
    random_bytes.argtypes = [ctypes.c_void_p, ctypes.c_int]
    random_bytes.restype = ctypes.c_int
    output = ctypes.create_string_buffer(32)
    if random_bytes(output, len(output)) != 1:
        raise RuntimeError("shared RAND_bytes failed")
    print("SHARED_RAND_BYTES=passed", flush=True)
    try:
        supported = library.RAND_snapsafe_is_supported
    except AttributeError as error:
        raise RuntimeError("shared library does not export RAND_snapsafe_is_supported") from error
    supported.argtypes = []
    supported.restype = ctypes.c_int
    support = supported()
    if support not in (0, 1):
        raise RuntimeError(f"invalid snapshot support result: {support}")
    if not pathlib.Path("/dev/sysgenid").exists() and support != 0:
        raise RuntimeError("snapshot support reported without generation device")
    print(f"SNAPSAFE_DEVICE_SUPPORTED={support}", flush=True)
    print("Real device generation and snapshot restoration remain unqualified.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=pathlib.Path)
    parser.add_argument("patch", type=pathlib.Path)
    parser.add_argument("artifacts", type=pathlib.Path)
    parser.add_argument("--export-probe", action="store_true")
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("this qualification requires native x86-64 Linux")
    archive = verified_file(
        args.archive,
        "a8f84a39918ec6415ce765d9b429d313ba97b8143169c172e734b9514464f5b2",
        64 * 1024**2,
    )
    patch = verified_file(
        args.patch,
        "3e68346bd5f9fc4e0b331647526ef20371061418846df821984100ec328b097e",
        1024**2,
    )
    args.artifacts.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="shared-", dir=args.artifacts.resolve()))
    print(f"ARTIFACTS={work}", flush=True)
    with tarfile.open(archive, "r:gz") as package:
        if sum(member.size for member in package.getmembers()) > 512 * 1024**2:
            raise ValueError("expanded source exceeds size limit")
        package.extractall(work, filter="data")
    tree = work / "openssl-3.5.8"
    patches = [patch]
    if args.export_probe:
        patches.append(pathlib.Path(__file__).with_name("snapsafe-export.patch").resolve())
    for source in patches:
        subprocess.run(
            ["patch", "--batch", "--fuzz=0", "-p1", "-i", str(source)],
            cwd=tree, check=True,
        )
    subprocess.run(["./Configure", "linux-x86_64", "shared", "no-fips"], cwd=tree, check=True)
    subprocess.run(["make", "-j1", "build_libs"], cwd=tree, check=True)
    probe(tree)


if __name__ == "__main__":
    main()
