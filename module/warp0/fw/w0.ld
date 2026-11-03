/* Warp0 crypto image: a flat image at Warp address 0, Warp0's local RAM (w0abi.h: one cycle per instruction, load
 * and store), cache off. .text (start.S first), .rodata, .data; .bss after the image, zeroed by start.S (objcopy -O
 * binary drops it). Stack: the top 4 KB of the 48 KB. */
ENTRY(_start)
SECTIONS
{
  . = 0x0;
  .text   : { *(.text.boot) *(.text .text.*) }
  .rodata : { *(.rodata .rodata.*) }
  .data   : { *(.data .data.*) *(.sdata .sdata.*) }
  . = ALIGN(4);
  _bss_start = .;
  .bss    : { *(.sbss .sbss.*) *(.bss .bss.*) *(COMMON) }
  . = ALIGN(4);
  _bss_end = .;
  _lbss_start = _bss_end;
  _lbss_end = _bss_end;
  ASSERT(_bss_end <= 0xB000, "firmware + bss run into the stack")
  _stack_top = 0xC000;
  _cop3 = 0;
  /DISCARD/ : { *(.MIPS.abiflags) *(.reginfo) *(.gnu.attributes) *(.comment) *(.pdr) *(.mdebug .mdebug.*) *(.note .note.*) }
}
