# Validates the ARM execution image metadata and archive configuration.
{
  pkgs,
  worker,
  image,
}:
assert image.imageName == "telchar-microvm-execution";
assert image.imageTag == worker.version;
assert image.imageConfig.User == "0:0";
assert image.imageConfig.Entrypoint == [ "/bin/telchar-microvm-execution" ];
assert
  image.imageConfig.Labels."org.opencontainers.image.source" == "https://github.com/techcyte/telchar";
pkgs.runCommand "telchar-microvm-image-contract" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  python3 - ${image} ${worker.version} <<'PY'
  import json
  import sys
  import tarfile

  with tarfile.open(sys.argv[1]) as archive:
      manifest = json.load(archive.extractfile("manifest.json"))
      assert len(manifest) == 1
      assert manifest[0]["RepoTags"] == ["telchar-microvm-execution:" + sys.argv[2]]
      config = json.load(archive.extractfile(manifest[0]["Config"]))
      assert config["architecture"] == "arm64"
      assert config["os"] == "linux"
      assert config["config"]["User"] == "0:0"
      trust = next(value.removeprefix("SSL_CERT_FILE=")
                   for value in config["config"]["Env"] if value.startswith("SSL_CERT_FILE="))
      found = False
      for layer in manifest[0]["Layers"]:
          with tarfile.open(fileobj=archive.extractfile(layer)) as filesystem:
              for member in filesystem:
                  if member.name.lstrip("./") == trust.lstrip("/") and member.isfile():
                      bundle = filesystem.extractfile(member).read()
                      assert 0 < len(bundle) <= 1048576
                      assert bundle.count(b"-----BEGIN CERTIFICATE-----") > 1
                      found = True
      assert found, "configured CA bundle is missing from image layers"
      assert config["config"]["Entrypoint"] == ["/bin/telchar-microvm-execution"]
      assert config["config"]["Labels"]["org.opencontainers.image.source"] == "https://github.com/techcyte/telchar"
  PY
  touch "$out"
''
