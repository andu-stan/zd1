# Warp0 firmware entry, every image (the linker script places it: the local RAM's start for an image that runs
# there, the image's SDRAM part for one that runs from SDRAM). The ARM has invalidated the I-cache (inval.S) before
# the start. Cache mode from the linker script's _cop3 (COP3 register 0: 0 = off, for code in local RAM, where it runs
# at one cycle an instruction without it; 5 = on, for code in SDRAM; LLVM has no mtc3: .word). Stack, zero .bss and
# the local RAM's .bss (.bss.lram; an empty range in an image that is all local), main(). main never returns; if
# it does, spin.
    .set noreorder
    .set noat
    .section .text.boot, "ax"
    .globl _start
_start:
    lui   $8, %hi(_cop3)
    addiu $8, $8, %lo(_cop3)
    .word 0x4c880000                        # mtc3 $8, $0
    nop
    nop
    lui   $29, %hi(_stack_top)
    addiu $29, $29, %lo(_stack_top)
    lui   $8, %hi(_bss_start)
    addiu $8, $8, %lo(_bss_start)
    lui   $9, %hi(_bss_end)
    addiu $9, $9, %lo(_bss_end)
1:  beq   $8, $9, 2f
    nop
    sw    $0, 0($8)
    b     1b
    addiu $8, $8, 4
2:  lui   $8, %hi(_lbss_start)
    addiu $8, $8, %lo(_lbss_start)
    lui   $9, %hi(_lbss_end)
    addiu $9, $9, %lo(_lbss_end)
3:  beq   $8, $9, 4f
    nop
    sw    $0, 0($8)
    b     3b
    addiu $8, $8, 4
4:  jal   main
    nop
5:  b     5b
    nop
