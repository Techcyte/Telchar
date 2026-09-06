"""Render cache access settings and atomically install protected credentials."""
import base64
import binascii
import os
import tempfile
from urllib.parse import urlsplit


def render(url, public_key, username, token, netrc_file, public_key_name=""):
    endpoint = urlsplit(url)
    if (endpoint.scheme != "https" or not endpoint.hostname or endpoint.username
            or endpoint.password or endpoint.query or endpoint.fragment
            or any(character.isspace() for character in url)):
        raise ValueError("cache URL must be HTTPS without credentials, query or fragment")
    public_key = public_key.strip()
    if public_key_name:
        if ":" in public_key_name or any(c.isspace() for c in public_key_name):
            raise ValueError("cache signing key name must not contain whitespace or colon")
        public_key = f"{public_key_name}:{public_key}"
    name, separator, encoded = public_key.partition(":")
    try:
        key = base64.b64decode(encoded, validate=True)
    except binascii.Error as error:
        raise ValueError("cache signing key must contain a base64 Ed25519 key") from error
    if not separator or not name or len(key) != 32 or any(c.isspace() for c in public_key):
        raise ValueError("cache signing key must be name:base64 Ed25519 key")
    if not username or any(c.isspace() for c in username):
        raise ValueError("cache username must be nonempty without whitespace")
    if not token or any(c.isspace() for c in token):
        raise ValueError("cache token must be nonempty without whitespace")
    if not os.path.isabs(netrc_file) or any(c.isspace() for c in netrc_file):
        raise ValueError("cache netrc path must be absolute without whitespace")
    return (
        f"extra-substituters = {url}\nextra-trusted-public-keys = {public_key}\n"
        f"netrc-file = {netrc_file}\n",
        f"machine {endpoint.hostname} login {username} password {token}\n",
    )


def write(path, content):
    directory = os.path.dirname(path)
    os.makedirs(directory, mode=0o700, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".telchar-candidate.", dir=directory, text=True)
    try:
        with os.fdopen(descriptor, "w") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, 0o400)
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise
