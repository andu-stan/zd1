# The telnet hook stock-patch/patch.py splices into the stock MCU firmware, from its AVR32 source (stock-patch/build.sh
# copies it back; it is committed): telnet_hook.S -> telnet_hook.bin, linked at the cave (hook.ld).
{
  stdenvNoCC,
  avr32-toolchain,
  src,
}:
stdenvNoCC.mkDerivation {
  name = "zd1-telnet-hook";
  src = src [
    "stock-patch/telnet_hook.S"
    "stock-patch/hook.ld"
  ];
  sourceRoot = "source/stock-patch";
  nativeBuildInputs = [ avr32-toolchain ];
  buildPhase = ''
    avr32-as -march=ucr2 -o telnet_hook.o telnet_hook.S
    avr32-ld -T hook.ld -o telnet_hook.elf telnet_hook.o
    avr32-objcopy -O binary telnet_hook.elf telnet_hook.bin
  '';
  installPhase = "install -Dm644 -t $out telnet_hook.bin";
}
