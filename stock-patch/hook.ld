/* Link the telnet hook blob at the free-flash cave address. */
ENTRY(telnet_hook)
SECTIONS
{
  . = 0x80038100;
  .text : { *(.text) *(.text.*) }
}
