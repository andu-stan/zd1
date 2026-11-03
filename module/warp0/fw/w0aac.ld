/* Warp0 AAC image (aac_main.c): two parts. Local RAM (Warp 0, one cycle a load): the tables the decoder reads in
 * full every frame (.rodata.lram, AAC_FAST) and its working set (.bss.lram, zeroed by start.S); the stack is
 * the top 4 KB. SDRAM at W0_AAC_SDRAM (code through the I-cache, cold tables, .bss). objcopy -j .lram ->
 * w0aac_local.bin, -j .text -j .rodata -j .data -> w0aac_sdram.bin. */
ENTRY(_start)
SECTIONS
{
  . = 0x0;
  .lram   : { *(.rodata.lram .rodata.lram.*) *(.data.lram .data.lram.*) }
  . = ALIGN(4);
  _lbss_start = .;
  .lbss (NOLOAD) : { *(.bss.lram .bss.lram.*) }
  . = ALIGN(4);
  _lbss_end = .;
  ASSERT(_lbss_end <= 0xB000, "local RAM: tables + working set run into the stack")
  _stack_top = 0xC000;
  _cop3 = 5;

  . = 0x61F90000;
  .text   : { *(.text.boot) *(.text .text.*) }
  .rodata : { *(.rodata .rodata.*) }
  .data   : { *(.data .data.*) *(.sdata .sdata.*) }
  . = ALIGN(4);
  _bss_start = .;
  .bss (NOLOAD) : { *(.sbss .sbss.*) *(.bss .bss.*) *(COMMON) }
  . = ALIGN(4);
  _bss_end = .;
  ASSERT(_bss_end <= 0x61FC0000, "SDRAM part runs into the Warp1 window")
  /DISCARD/ : { *(.MIPS.abiflags) *(.reginfo) *(.gnu.attributes) *(.comment) *(.pdr) *(.mdebug .mdebug.*) *(.note .note.*) }
}
