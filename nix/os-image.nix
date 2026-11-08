# The module OS images (module/os/assemble.py assemble, from os.toml): the system image, the rescue, the flashable
# os.bcd and os-debug.bcd, the emulator's dev payload, and the test unit's NAND with each installed.
{
  stdenvNoCC,
  fetchurl,
  python3,
  nix,
  src,
  parts,
  test-unit,
  version,
  built,
}:
let
  # B&W's last firmware for the Zeppelin Air with Lightning (2.04.35): only its module image's wrapper (HWID 0xBE,
  # the Lightning's CnE and resources) is used, around our payload: os-zd1l.bcd (assemble.py, lightning). The
  # archive.org item downloads/manifest.json lists.
  zd1l-fw = fetchurl {
    url = "https://archive.org/download/bw-zeppelin-air-software/zeppelin-air-lightning/zd1lv20435.fw";
    sha256 = "757a48bbf6fd7b531eedb56aee64f6799ce225874649ead179e7e00c2006d6c9";
  };
in
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
  nativeBuildInputs = [
    python3
    nix # nix-hash: the boot image's and the rescue's identity by content (assemble.py, content_id)
  ];
  buildPhase = ''
    python3 -I module/os/assemble.py assemble --parts ${parts} --base-nand ${test-unit}/nand.bin \
      --version ${version} --built "${built}" --out $out --zd1l-fw ${zd1l-fw}
  '';
  dontInstall = true;
  dontFixup = true;
}
