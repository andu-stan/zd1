# The module OS images (module/os/assemble.py assemble, from os.toml): the system image, the rescue, the flashable
# os.bcd and os-debug.bcd, the emulator's dev payload, and the test unit's NAND with each installed.
{
  stdenvNoCC,
  python3,
  src,
  parts,
  test-unit,
  version,
  built,
}:
stdenvNoCC.mkDerivation {
  name = "zd1-os-image";
  src = src [
    "module/os/assemble.py"
    "module/os/os.toml"
    "module/os/mkcpio.py"
    "module/os/rootfs"
    "module/os/mkpayload.py"
    "tools/zeppelin/__init__.py"
    "tools/zeppelin/fw/bcd.py"
    "tools/zeppelin/fw/mkbcd.py"
    "blobs/module/app_DMP.bcd"
    "blobs/module/nokia-hd1"
  ];
  nativeBuildInputs = [ python3 ];
  buildPhase = ''
    python3 -I module/os/assemble.py assemble --parts ${parts} --base-nand ${test-unit}/nand.bin \
      --version ${version} --built "${built}" --out $out
  '';
  dontInstall = true;
  dontFixup = true;
}
