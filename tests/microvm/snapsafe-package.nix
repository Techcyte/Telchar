# Isolated package qualification; not an execution-image dependency override.
{
  pkgs,
  archive,
  patch,
}:
assert pkgs.stdenv.hostPlatform.isLinux;
assert builtins.hashFile "sha256" archive == "a8f84a39918ec6415ce765d9b429d313ba97b8143169c172e734b9514464f5b2";
assert builtins.hashFile "sha256" patch == "3e68346bd5f9fc4e0b331647526ef20371061418846df821984100ec328b097e";
pkgs.openssl_3_5.overrideAttrs (previous: {
  version = "3.5.8";
  src = builtins.path {
    path = archive;
    name = "openssl-3.5.8.tar.gz";
  };
  patches = [
    patch
    ./snapsafe-export.patch
    ./snapsafe-cert-file.patch
    ./snapsafe-kernel-detection.patch
    (pkgs.path + "/pkgs/development/libraries/openssl/3.5/use-etc-ssl-certs.patch")
  ];
  patchFlags = [ "-p1" "--fuzz=0" ];
  configureFlags = previous.configureFlags ++ [ "no-fips" ];
  enableParallelBuilding = false;
})
