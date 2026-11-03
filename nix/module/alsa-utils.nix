# aplay, speaker-test and amixer (os.toml [alsa] tools), static: the debug image's audio tools. $out/bin.
{
  lib,
  stdenv,
  fetchurl,
  pkg-config,
  alsa-lib,
  os,
}:
let
  a = os.alsa;
in
stdenv.mkDerivation {
  pname = "alsa-utils";
  inherit (a) version;
  src = fetchurl { inherit (a.utils) url sha256; };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ alsa-lib ];
  configureFlags = [
    "--disable-alsamixer"
    "--disable-alsaconf"
    "--disable-alsaloop"
    "--disable-bat"
    "--disable-xmlto"
    "--disable-rst2man"
    "--disable-nls"
    "--with-udev-rules-dir=/tmp/udev"
    "--with-systemdsystemunitdir=no"
  ];
  env = {
    CFLAGS = "-Os";
    LIBS = "-lpthread -lm -ldl";
  };
  enableParallelBuilding = true;
  stripAllList = [ "bin" ]; # symbols too: the rootfs is RAM

  # only the tools os.toml lists
  buildPhase = ''
    runHook preBuild
    for t in ${lib.concatStringsSep " " a.tools}; do make -C $t -j$NIX_BUILD_CORES; done
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    for t in ${lib.concatStringsSep " " a.tools}; do install -Dm755 $t/$t $out/bin/$t; done
    runHook postInstall
  '';
}
