# Microchip's AVR32 GNU toolchain 3.4.2 (GCC 4.4.7; LLVM has no AVR32 backend): an x86_64 Linux binary that Microchip
# no longer serves at a stable URL. The archive.org copy is the unpacked avr32-gnu-toolchain-linux_x86_64/ repacked.
{
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  stdenv,
  zlib,
  ncurses5,
}:
stdenvNoCC.mkDerivation {
  pname = "avr32-gnu-toolchain";
  version = "3.4.2";
  src = fetchurl {
    url = "https://archive.org/download/avr32-gnu-toolchain-3.4.2-linux-x86_64/avr32-toolchain.tar.gz";
    sha256 = "dc6b37c5f31cb997e7a067c414309f1e11bb79be16ce9f954047353df50f8ad7";
  };
  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [
    stdenv.cc.cc.lib
    zlib
    ncurses5
  ];
  autoPatchelfIgnoreMissingDeps = true;
  dontStrip = true;
  installPhase = "cp -r . $out";
  meta.platforms = [ "x86_64-linux" ];
}
