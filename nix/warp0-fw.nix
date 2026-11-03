# The Warp0 firmware images (MIPS I, LLVM on the host; module/warp0/build.py): what the zd1-warp0 crate embeds.
{
  stdenvNoCC,
  python3,
  llvm,
  src,
}:
stdenvNoCC.mkDerivation {
  name = "zd1-warp0-fw";
  inherit src;
  nativeBuildInputs = [ python3 ];
  ZD1_LLVM = llvm;
  buildPhase = "W0_OUT=$out python3 -I module/warp0/build.py";
  dontInstall = true;
}
