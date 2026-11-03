/* linuxboot stub (module/os/mkpayload.py): copied to 0x61F00000 by the trampoline at the start of the app image, so the
 * Linux Image can be moved down to 0x60008000 (where the preloader enters). Stack above it, 0x61F00000-0x62000000. */
ENTRY(_start)
SECTIONS
{
  . = 0x61F00000;
  .text : { KEEP(*(.text.boot)) *(.text .text.*) *(.rodata .rodata.*) }
  .data : { . = ALIGN(4); *(.data .data.*) }
  .bss  : { . = ALIGN(4); _bss_start = .; *(.bss .bss.*) *(COMMON) . = ALIGN(4); _bss_end = .; }
  . = ALIGN(8);
  . = . + 0x10000;
  _stack_top = .;
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) *(.note*) }
}
