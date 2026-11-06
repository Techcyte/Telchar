"""Readonly retained binds preserve inherited namespace mount restrictions."""
import importlib.util
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("retained_store", ROOT / "deploy/microvm/retained_store.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class MountRestrictionTests(unittest.TestCase):
    def test_readonly_bind_preserves_restrictions(self):
        for restrictions in (0, os.ST_NOSUID | os.ST_NODEV, os.ST_NOEXEC):
            with self.subTest(restrictions=restrictions), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                retained = root / "inputs"
                database = retained / "nix/var/nix/db/db.sqlite"
                database.parent.mkdir(parents=True)
                (retained / "nix/store").mkdir()
                sqlite3.connect(database).close()
                stat = os.statvfs(retained)
                fields = list(stat)
                fields[8] = restrictions
                with patch.object(module, "mount") as mount, patch.object(module, "unmount"), patch.object(
                    module.os, "statvfs", return_value=os.statvfs_result(fields)
                ):
                    with module.job_store(retained, root / "job"):
                        readonly = [call for call in mount.call_args_list if call.args[0] is None]
                        self.assertEqual(len(readonly), 2)
                        for call in readonly:
                            self.assertEqual(call.kwargs["flags"], 4096 | 32 | 1 | restrictions)


if __name__ == "__main__":
    unittest.main()
