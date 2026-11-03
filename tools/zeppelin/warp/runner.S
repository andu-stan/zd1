# Warp1 job runner (docs/module/warp.md): runs a list of bus operations prepared by the ARM in SDRAM, so the ARM
# can drive Sol through Warp1 (Sol only runs while Warp1 does). MIPS I, little-endian, linked at 0x61FC0000.
# Every load delay and branch delay slot is filled by hand (the real core is an R3000-style MIPS I: GCC 4.1.2
# output fills them too). The cache is left off (COP3 register 0 untouched): every access goes to SDRAM, so the ARM
# sees results through the uncached alias 0x7xxxxxxx without cache maintenance.
#
#   0x61FC0F00 status: +0 'DONE' (0x444F4E45) at the end, +4 number of results, +8 address of the current command
#   0x61FC1000 job: commands of 4 words {op, a, b, c}
#   0x61FC8000 results, one word each
# ops: 0 end, 1 store32 [a] = b, 2 store16, 3 store8, 4 result = load32 [a], 5 result = load16 [a],
#      6 poll16: until ([a] & b) != 0 or c tries; result = tries left (0 = timed out), 7 copy c words from a to b,
#      8 delay c loops, 9 store32 b to each of the c addresses listed at a, 10 call a (code that returns with
#      `jr $31`; it may use $1-$15, $24, $25 and add results with `sw x, 0($17)`, `addiu $17, $17, 4`,
#      `addiu $18, $18, 1`; $16-$19 belong to the runner), 11 fill: c words from a with b, b+1, b+2, ...,
#      12 diff: compare c words from a with b, b+1, ...: result = the number of mismatches, then (address, value) for
#      each (at most 16)
    .set noreorder
    .set noat
    .text
    .globl _start
_start:
    lui   $19, 0x61fc
    ori   $19, $19, 0x0f00          # s3 = status
    lui   $16, 0x61fc
    ori   $16, $16, 0x1000          # s0 = job
    lui   $17, 0x61fc
    ori   $17, $17, 0x8000          # s1 = results
    addu  $18, $0, $0               # s2 = count
    sw    $0, 0($19)
    sw    $0, 4($19)
next:
    sw    $16, 8($19)
    lw    $8, 0($16)                # t0 = op
    lw    $4, 4($16)                # a0 = a
    lw    $5, 8($16)                # a1 = b
    lw    $6, 12($16)               # a2 = c
    addiu $16, $16, 16
    addiu $9, $0, 1
    beq   $8, $9, w32
    addiu $9, $0, 2
    beq   $8, $9, w16
    addiu $9, $0, 3
    beq   $8, $9, w8
    addiu $9, $0, 4
    beq   $8, $9, r32
    addiu $9, $0, 5
    beq   $8, $9, r16
    addiu $9, $0, 6
    beq   $8, $9, poll16
    addiu $9, $0, 7
    beq   $8, $9, copy32
    addiu $9, $0, 8
    beq   $8, $9, delay
    addiu $9, $0, 9
    beq   $8, $9, storelist
    addiu $9, $0, 10
    beq   $8, $9, call
    addiu $9, $0, 11
    beq   $8, $9, fill
    addiu $9, $0, 12
    beq   $8, $9, diff
    nop
done:
    sw    $18, 4($19)
    lui   $8, 0x444f
    ori   $8, $8, 0x4e45
    sw    $8, 0($19)
halt:
    beq   $0, $0, halt
    nop
w32:
    sw    $5, 0($4)
    beq   $0, $0, next
    nop
w16:
    sh    $5, 0($4)
    beq   $0, $0, next
    nop
w8:
    sb    $5, 0($4)
    beq   $0, $0, next
    nop
r32:
    lw    $10, 0($4)
    nop
    beq   $0, $0, result
    nop
r16:
    lhu   $10, 0($4)
    nop
    beq   $0, $0, result
    nop
poll16:
    lhu   $10, 0($4)
    nop
    and   $10, $10, $5
    bne   $10, $0, polled
    nop
    addiu $6, $6, -1
    bne   $6, $0, poll16
    nop
polled:
    addu  $10, $6, $0
result:
    sw    $10, 0($17)
    addiu $17, $17, 4
    beq   $0, $0, next
    addiu $18, $18, 1
copy32:
    beq   $6, $0, next
    nop
    lw    $10, 0($4)
    nop
    sw    $10, 0($5)
    addiu $4, $4, 4
    addiu $5, $5, 4
    beq   $0, $0, copy32
    addiu $6, $6, -1
delay:
    bne   $6, $0, delay
    addiu $6, $6, -1
    beq   $0, $0, next
    nop
storelist:
    beq   $6, $0, next
    nop
    lw    $10, 0($4)
    nop
    sw    $5, 0($10)
    addiu $4, $4, 4
    beq   $0, $0, storelist
    addiu $6, $6, -1
call:
    jalr  $31, $4
    nop
    beq   $0, $0, next
    nop
fill:
    beq   $6, $0, next
    nop
    sw    $5, 0($4)
    addiu $5, $5, 1
    addiu $4, $4, 4
    beq   $0, $0, fill
    addiu $6, $6, -1
diff:
    addu  $11, $17, $0               # the count goes here
    addiu $17, $17, 4
    addiu $18, $18, 1
    addu  $12, $0, $0
diffloop:
    beq   $6, $0, diffend
    nop
    lw    $10, 0($4)
    nop
    beq   $10, $5, diffskip
    nop
    addiu $12, $12, 1
    sltiu $13, $12, 17
    beq   $13, $0, diffskip
    nop
    sw    $4, 0($17)
    sw    $10, 4($17)
    addiu $17, $17, 8
    addiu $18, $18, 2
diffskip:
    addiu $5, $5, 1
    addiu $4, $4, 4
    beq   $0, $0, diffloop
    addiu $6, $6, -1
diffend:
    beq   $0, $0, next
    sw    $12, 0($11)
