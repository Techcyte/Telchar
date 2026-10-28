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
      assert config["config"]["Entrypoint"] == ["/bin/telchar-microvm-execution"]
      assert config["config"]["Labels"]["org.opencontainers.image.source"] == "https://github.com/techcyte/telchar"
  PY
  touch "$out"
''
