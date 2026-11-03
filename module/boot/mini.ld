/* mini (BridgeCo DM870, ARM926EJ-S): stage-2 image, loaded and entered at 0x60008000 by the ROM preloader (or by
 * a loader in the emulator). SDRAM 0x60000000-0x62000000, MMU off, flat. */
ENTRY(_start)
SECTIONS
{
  . = 0x60008000;
  .text : { KEEP(*(.text.boot)) *(.text .text.*) *(.rodata .rodata.*) }
  .data : { . = ALIGN(4); *(.data .data.*) }
  .bss  : { . = ALIGN(4); _bss_start = .; *(.bss .bss.*) *(COMMON) . = ALIGN(4); _bss_end = .; }
  . = ALIGN(8);
  . = . + 0x10000;        /* 64 KiB stack */
  _stack_top = .;
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) *(.note*) }
}
