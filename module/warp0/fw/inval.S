# The I-cache invalidate stub, at W0_INVAL (docs/module/warp.md section 3). The cache survives a Warp reset, so
# new firmware at an address that ran before would execute stale lines; this stub always has the same content, so
# a stale copy of it is still right. Cache off with the cache RAM mapped (COP3 = 0x10), zero the 256 tags
# (0x02000000-0x020005FF), cache off (COP3 = 0), store the marker word after the stub, spin. The ARM waits for the
# marker, halts the core, loads the firmware and starts it.
    .set noreorder
    .set noat
    .text
    .globl _start
_start:
    li    $8, 0x10
    .word 0x4c880000                        # mtc3 $8, $0
    nop
    nop
    lui   $9, 0x0200
    lui   $10, 0x0200
    ori   $10, $10, 0x0600
1:  sw    $0, 0($9)
    addiu $9, $9, 4
    bne   $9, $10, 1b
    nop
    .word 0x4c800000                        # mtc3 $0, $0: cache off (start.S turns it on)
    nop
    nop
    lui   $9, %hi(marker)
    lui   $8, 0x494E
    ori   $8, $8, 0x5644                    # "INVD"
    sw    $8, %lo(marker)($9)
2:  b     2b
    nop
marker:
    .word 0
