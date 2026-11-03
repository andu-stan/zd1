# The MCU firmware (mcu/Makefile): $out/NAME.{elf,hex,bin,map}.
#   emulator     the emulator's build (packages mcu-emu): the console on USART0 as well, where zemu reads and types
#                it (no USB host there; ZD1_EMU)
{
  lib,
  stdenv,
  avr32-toolchain,
  src,
  name ? "zd1",
  emulator ? false,
}:
stdenv.mkDerivation {
  inherit name;
  src = src [
    "mcu/Makefile"
    "mcu/src"
    "mcu/config"
    "mcu/dfp-include"
    "mcu/asf"
    "mcu/link_zd1.lds"
  ];
  sourceRoot = "source/mcu";
  nativeBuildInputs = [ avr32-toolchain ];
  makeFlags = [ "OUT=${name}" ] ++ lib.optional emulator "EXTRA_CFLAGS=-DZD1_EMU";
  enableParallelBuilding = true;
  installPhase = ''
    mkdir -p $out
    cp build/${name}.{elf,hex,bin,map} $out/
  '';
  dontFixup = true;
  meta.platforms = [ "x86_64-linux" ];
}
