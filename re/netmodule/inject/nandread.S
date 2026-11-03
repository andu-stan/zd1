    .syntax unified
    .thumb
    .global pl
    .thumb_func
pl:
    push {r0, r1, r2, r3, r4, lr}   @ save incoming args (this/argv) + scratch + lr
    @ --- our work: flashRead(dev=0, addr=0, dst, len=0x40) ---
    movs r0, #0                     @ device 0 = NAND
    movs r1, #0                     @ flash addr 0 (BL1 start)
    ldr  r2, =0xDD000000            @ DSTBUF (patched)
    movs r3, #0x40                  @ 64 bytes
    ldr  r4, =0x6017e79c            @ flashRead (ARM, even -> blx switches mode)
    blx  r4
    @ --- chain to original execute so the command finishes as stock ---
    ldr  r0, [sp, #0]
    ldr  r1, [sp, #4]
    ldr  r2, [sp, #8]
    ldr  r3, [sp, #12]
    ldr  r4, =0xEE000007            @ ORIG_EXEC = original vtable[7] (patched; Thumb odd)
    blx  r4
    add  sp, sp, #20                @ drop saved r0-r4 (keep r0 = original's return)
    pop  {pc}                       @ return (sp now at saved lr)
    .ltorg
