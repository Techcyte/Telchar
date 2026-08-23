#!/usr/bin/env python3
import datetime
import pathlib

import tomllib

root = pathlib.Path(__file__).resolve().parent.parent
with (root / "deny.toml").open("rb") as handle:
    deny = tomllib.load(handle)
with (root / "security/advisory-exceptions.toml").open("rb") as handle:
    policy = tomllib.load(handle)

ignored = set()
for entry in deny.get("advisories", {}).get("ignore", []):
    ignored.add(entry if isinstance(entry, str) else entry.get("id"))

exceptions = policy.get("exceptions", [])
exception_ids = set()
today = datetime.date.today()
for exception in exceptions:
    advisory_id = exception.get("id")
    reason = exception.get("reason")
    expires = exception.get("expires")
    if not advisory_id or not reason or not isinstance(expires, datetime.date):
        raise SystemExit(
            "advisory exceptions require id, reason, and ISO expiration date"
        )
    if expires < today:
        raise SystemExit(f"advisory exception expired: {advisory_id} ({expires})")
    if advisory_id in exception_ids:
        raise SystemExit(f"duplicate advisory exception: {advisory_id}")
    exception_ids.add(advisory_id)

if ignored != exception_ids:
    raise SystemExit(
        "deny.toml advisory ignores and security/advisory-exceptions.toml differ: "
        f"ignored={sorted(ignored)} exceptions={sorted(exception_ids)}"
    )

print(f"advisory exceptions valid: {len(exception_ids)}")
