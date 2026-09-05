# Verifies cache credential rendering and protected atomic file replacement.
import base64
import importlib.util
import netrc
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "cache_credentials", Path(__file__).parents[1] / "cache_credentials.py"
)
credentials = importlib.util.module_from_spec(spec)
spec.loader.exec_module(credentials)

KEY = "cache:" + base64.b64encode(bytes(range(32))).decode()


class CacheCredentialsTests(unittest.TestCase):
    def test_render_authenticated_cache_without_token_in_configuration(self):
        config, netrc = credentials.render(
            "https://cache.example.com/cache", KEY, "reader", "opaque-token", "/state/netrc"
        )
        self.assertIn("extra-substituters = https://cache.example.com/cache\n", config)
        self.assertIn(f"extra-trusted-public-keys = {KEY}\n", config)
        self.assertIn("netrc-file = /state/netrc\n", config)
        self.assertNotIn("opaque-token", config)
        self.assertEqual(netrc, "machine cache.example.com login reader password opaque-token\n")

    def test_render_key_with_explicit_name(self):
        config, _ = credentials.render(
            "https://cache.example.com/cache", KEY.split(":", 1)[1],
            "reader", "token", "/state/netrc", public_key_name="cache"
        )
        self.assertIn(f"extra-trusted-public-keys = {KEY}\n", config)
        for name in ["bad name", "cache\nsetting", "cache:other"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                credentials.render(
                    "https://cache.example.com/cache", KEY.split(":", 1)[1],
                    "reader", "token", "/state/netrc", public_key_name=name
                )

    def test_reject_unusable_credentials_before_replacing_files(self):
        for url, key, token in [
            ("http://cache.example.com/cache", KEY, "token"),
            ("https://user@cache.example.com/cache", KEY, "token"),
            ("https://cache.example.com/cache?secret=x", KEY, "token"),
            ("https://cache.example.com/cache", "cache:broken", "token"),
            ("https://cache.example.com/cache", KEY, ""),
            ("https://cache.example.com/cache", KEY, "token\n machine other"),
        ]:
            with self.subTest(url=url, key=key, token=token):
                with self.assertRaises(ValueError):
                    credentials.render(url, key, "reader", token, "/state/netrc")

    def test_username_is_required_and_cannot_inject_netrc_fields(self):
        for username in ["", "reader\npassword other", "reader other"]:
            with self.subTest(username=username), self.assertRaises(ValueError):
                credentials.render("https://cache.example.com/cache", KEY, username, "token", "/state/netrc")

    def test_credentials_are_readable_by_netrc_parser(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "netrc")
            _, content = credentials.render("https://cache.example.com/cache", KEY, "service", "secret", path)
            credentials.write(path, content)
            self.assertEqual(netrc.netrc(path).authenticators("cache.example.com"), ("service", "", "secret"))

    def test_rotation_preserves_permissions_and_replaces_symlink_not_target(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "netrc"
            external = Path(directory) / "untouched"
            external.write_text("keep")
            target.symlink_to(external)
            credentials.write(str(target), "first")
            self.assertEqual(external.read_text(), "keep")
            self.assertFalse(target.is_symlink())
            self.assertEqual(target.stat().st_mode & 0o777, 0o400)
            credentials.write(str(target), "second")
            self.assertEqual(target.read_text(), "second")
            self.assertEqual(sorted(p.name for p in Path(directory).iterdir()), ["netrc", "untouched"])


if __name__ == "__main__":
    unittest.main()
