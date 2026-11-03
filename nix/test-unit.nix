# The emulator's test unit, factory-fresh (tools/zeppelin/recover/mkunit.py): MCU flash, user page, EEPROM and NAND
# (BL1, BL2, the stock app, PersParam with this MAC), from blobs/ alone. A made-up MAC: nothing per-unit.
{
  stdenvNoCC,
  python3,
  src,
  unitMac ? "EC:66:D1:00:00:42",
}:
stdenvNoCC.mkDerivation {
  name = "zd1-test-unit";
  src = src [
    "tools/zeppelin/__init__.py"
    "tools/zeppelin/paths.py"
    "tools/zeppelin/fw/bcd.py"
    "tools/zeppelin/fw/ihex.py"
    "tools/zeppelin/recover"
    "blobs/mcu"
    "blobs/module"
  ];
  nativeBuildInputs = [ python3 ];
  buildPhase = "python3 -I tools/zeppelin/recover/mkunit.py --mac ${unitMac} --out $out";
  dontInstall = true;
}
