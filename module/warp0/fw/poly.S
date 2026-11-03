# Poly1305 blocks for Warp0 (main.c: poly_blocks), by hand: the compiler hoists the 25 multiplies and spills the
# 64-bit accumulators to the stack, which is uncached SDRAM here. 26-bit limbs as poly1305-donna-32; one 64-bit
# accumulator ($3:$2) at a time, everything else in registers. MIPS I: every multu is at least two instructions
# after the last mflo/mfhi (the HI/LO hazard), load delay slots filled.
#
# void poly_blocks(poly_t *p, const u32 *m, u32 nblocks, u32 hibit)
#   p: r0-r4 at +0..+16, h0-h4 at +20..+36 (main.c poly_t); m: word aligned; hibit: 1 << 24 or 0
# registers: r0-r4 $8-$12, h0-h4 $13-$17, 5*r1..5*r4 $18-$21, mask 0x3ffffff $22, accumulator $3:$2,
#            product $23:$1, new limbs n0-n3 $24 $25 $30 $31
    .set noreorder
    .set noat
    .text
    .globl poly_blocks
poly_blocks:
    beqz  $6, 9f
    nop
    addiu $sp, $sp, -40
    sw    $16, 0($sp)
    sw    $17, 4($sp)
    sw    $18, 8($sp)
    sw    $19, 12($sp)
    sw    $20, 16($sp)
    sw    $21, 20($sp)
    sw    $22, 24($sp)
    sw    $23, 28($sp)
    sw    $30, 32($sp)
    sw    $31, 36($sp)
    lw    $8, 0($4)
    lw    $9, 4($4)
    lw    $10, 8($4)
    lw    $11, 12($4)
    lw    $12, 16($4)
    lw    $13, 20($4)
    lw    $14, 24($4)
    lw    $15, 28($4)
    lw    $16, 32($4)
    lw    $17, 36($4)
    sll   $18, $9, 2
    addu  $18, $18, $9
    sll   $19, $10, 2
    addu  $19, $19, $10
    sll   $20, $11, 2
    addu  $20, $20, $11
    sll   $21, $12, 2
    addu  $21, $21, $12
    lui   $22, 0x03ff
    ori   $22, $22, 0xffff
1:  lw    $2, 0($5)               # the block, little-endian words t0-t3 -> 26-bit limbs added to h
    lw    $3, 4($5)
    lw    $24, 8($5)
    lw    $25, 12($5)
    addiu $5, $5, 16
    and   $1, $2, $22
    addu  $13, $13, $1
    srl   $1, $2, 26
    sll   $23, $3, 6
    or    $1, $1, $23
    and   $1, $1, $22
    addu  $14, $14, $1
    srl   $1, $3, 20
    sll   $23, $24, 12
    or    $1, $1, $23
    and   $1, $1, $22
    addu  $15, $15, $1
    srl   $1, $24, 14
    sll   $23, $25, 18
    or    $1, $1, $23
    and   $1, $1, $22
    addu  $16, $16, $1
    srl   $1, $25, 8
    or    $1, $1, $7
    addu  $17, $17, $1
    move  $2, $0                  # h * r mod 2^130 - 5, limb by limb, carries in order
    move  $3, $0
    # d0
    multu $13, $8
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $14, $21
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $15, $20
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $16, $19
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $17, $18
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    and   $24, $2, $22
    srl   $2, $2, 26
    sll   $3, $3, 6
    or    $2, $2, $3
    move  $3, $0
    # d1
    multu $13, $9
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $14, $8
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $15, $21
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $16, $20
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $17, $19
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    and   $25, $2, $22
    srl   $2, $2, 26
    sll   $3, $3, 6
    or    $2, $2, $3
    move  $3, $0
    # d2
    multu $13, $10
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $14, $9
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $15, $8
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $16, $21
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $17, $20
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    and   $30, $2, $22
    srl   $2, $2, 26
    sll   $3, $3, 6
    or    $2, $2, $3
    move  $3, $0
    # d3
    multu $13, $11
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $14, $10
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $15, $9
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $16, $8
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $17, $21
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    and   $31, $2, $22
    srl   $2, $2, 26
    sll   $3, $3, 6
    or    $2, $2, $3
    move  $3, $0
    # d4
    multu $13, $12
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $14, $11
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $15, $10
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $16, $9
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    multu $17, $8
    mflo  $1
    mfhi  $23
    addu  $2, $2, $1
    sltu  $1, $2, $1
    addu  $3, $3, $23
    addu  $3, $3, $1
    and   $17, $2, $22             # h4, and the carry out of it times 5 into h0
    srl   $2, $2, 26
    sll   $3, $3, 6
    or    $2, $2, $3
    sll   $1, $2, 2
    addu  $2, $2, $1
    addu  $13, $24, $2
    srl   $1, $13, 26
    and   $13, $13, $22
    addu  $14, $25, $1
    move  $15, $30
    addiu $6, $6, -1
    bnez  $6, 1b
    move  $16, $31
    sw    $13, 20($4)
    sw    $14, 24($4)
    sw    $15, 28($4)
    sw    $16, 32($4)
    sw    $17, 36($4)
    lw    $16, 0($sp)
    lw    $17, 4($sp)
    lw    $18, 8($sp)
    lw    $19, 12($sp)
    lw    $20, 16($sp)
    lw    $21, 20($sp)
    lw    $22, 24($sp)
    lw    $23, 28($sp)
    lw    $30, 32($sp)
    lw    $31, 36($sp)
    nop
    addiu $sp, $sp, 40
9:  jr    $31
    nop
