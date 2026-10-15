// GetMatchesSpecN_2, SDK 26.03 Asm/x86/LzFindOpt.asm control-flow port.
// Upstream 0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17, public domain.
// Negative offsets from the exclusive end retain the upstream child-first
// mismatch path and long-match copy. Match extension uses bounded u64 reads.
// Zero heads and non-descending children retain the checked oracle's errors.
// A-F and integer/direct-mode guards are established by bt_kernel.rs.
// x0 end, w1 position, x3 tree, x4 length, x5 output, x7 heads;
// x11/x12 child links, w13/w14 cyclic position/size, x15/x16 input/output
// ends, x17 initial maximum, x19 result, w20/w21 initial/current cut,
// x22/x23 lengths, x24 maximum, x25 pair, w26 delta, w27 child,
// x28 match end, x2 scan offset, x6 output count. No x18 or calls.
sub sp, sp, #80
stp x19, x20, [sp]
stp x21, x22, [sp, #16]
stp x23, x24, [sp, #32]
stp x25, x26, [sp, #48]
stp x27, x28, [sp, #64]
ldr x16, [sp, #80]
ldr x15, [sp, #88]
ldr x13, [sp, #96]
ldr w14, [sp, #104]
ldr x19, [sp, #112]
mov w20, w4
sub x4, x0, x2
add x4, x4, #1
sub x17, x6, x4
Lneg_main:
ldr w26, [x7], #4
cbz w26, Lneg_error
add x0, x0, #1
mov x6, x5
add x5, x5, #4
add x11, x3, x13, lsl #3
sub w27, w1, w26
cmp w1, w14
b.lo Lneg_direct
cmp w26, w14
b.hs Lneg_empty
subs w25, w13, w26
csel w8, w14, wzr, lo
add w25, w25, w8
b Lneg_prepare
Lneg_direct:
cmp w26, w1
b.eq Lneg_empty
b.hi Lneg_error
mov w25, w27
Lneg_prepare:
add x12, x11, #4
neg x22, x4
mov x23, x22
mov x24, x17
mov w21, w20
.p2align 5
Lneg_tree:
sub x28, x0, x26
cmp x22, x23
csel x2, x22, x23, lo
add x25, x3, x25, lsl #3
ldr w8, [x25]
ldrb w8, [x28, x2]
ldrb w9, [x0, x2]
cmp w9, w8
b.eq Lneg_matched
b.lo Lneg_left_zero
str w27, [x11]
ldr w27, [x25, #4]
add x11, x25, #4
b Lneg_next
Lneg_left_zero:
str w27, [x12]
ldr w27, [x25]
mov x12, x25
Lneg_next:
sub w8, w1, w26
cmp w27, w8
b.hs Lneg_error
subs w21, w21, #1
b.eq Lneg_finish_tree
sub w26, w1, w27
cmp w1, w14
b.hs Lneg_cyclic
mov w25, w27
cbnz w27, Lneg_tree
b Lneg_finish_tree
Lneg_cyclic:
cmp w26, w14
b.hs Lneg_finish_tree
subs w25, w13, w26
csel w8, w14, wzr, lo
add w25, w25, w8
b Lneg_tree
.p2align 5
Lneg_matched:
adds x2, x2, #1
b.eq Lneg_full
// At least eight bytes remain iff the negative offset is <= -8.
Lneg_words:
cmn x2, #8
b.hi Lneg_tail
ldr x8, [x28, x2]
ldr x9, [x0, x2]
eor x8, x8, x9
cbnz x8, Lneg_word_diff
adds x2, x2, #8
b.eq Lneg_full
b Lneg_words
Lneg_word_diff:
rbit x8, x8
clz x8, x8
add x2, x2, x8, lsr #3
Lneg_tail:
ldrb w8, [x28, x2]
ldrb w9, [x0, x2]
cmp w9, w8
b.ne Lneg_mismatch
adds x2, x2, #1
b.ne Lneg_tail
b Lneg_full
Lneg_mismatch:
b.lo Lneg_left_match
str w27, [x11]
ldr w27, [x25, #4]
add x11, x25, #4
mov x23, x2
b Lneg_max
Lneg_left_match:
str w27, [x12]
ldr w27, [x25]
mov x12, x25
mov x22, x2
Lneg_max:
cmp x24, x2
b.hs Lneg_next
mov x24, x2
add w8, w2, w4
sub w9, w26, #1
stp w8, w9, [x5], #8
b Lneg_next
Lneg_finish_tree:
str wzr, [x12]
str wzr, [x11]
sub x8, x5, x6
lsr x8, x8, #2
sub w8, w8, #1
str w8, [x6]
b Lneg_advance
Lneg_empty:
str xzr, [x11]
str wzr, [x6]
Lneg_advance:
add w1, w1, #1
add w13, w13, #1
cmp x5, x16
b.hs Lneg_done
cmp x7, x15
b.ne Lneg_main
b Lneg_done
Lneg_full:
ldp w8, w9, [x25]
str w8, [x11]
str w9, [x12]
sub w10, w26, #1
stp w4, w10, [x5], #8
sub x8, x5, x6
lsr x8, x8, #2
sub w8, w8, #1
str w8, [x6]
add w1, w1, #1
add w13, w13, #1
cmp x7, x15
b.eq Lneg_done
cmp x5, x16
b.hs Lneg_done
ldr w8, [x7]
cmp w8, w26
b.ne Lneg_main
ldrb w8, [x28]
ldrb w9, [x0]
cmp w8, w9
b.ne Lneg_main
// Copy consecutive matching nodes, wrapping only the source. A bounds the
// destination run before the cyclic end; each copied node was initialized.
add x25, x3, x13, lsl #3
subs w8, w13, w26
csel w9, w14, wzr, lo
add w8, w8, w9
add x11, x3, x8, lsl #3
add x12, x3, x14, lsl #3
.p2align 5
Lneg_long:
add x7, x7, #4
ldr x8, [x11], #8
str x8, [x25], #8
cmp x11, x12
csel x11, x3, x11, eq
mov w8, #2
stp w8, w4, [x5]
str w10, [x5, #8]
add x5, x5, #12
add x0, x0, #1
add x28, x28, #1
add w1, w1, #1
add w13, w13, #1
cmp x7, x15
b.eq Lneg_done
ldr w8, [x7]
cmp w8, w26
b.ne Lneg_long_footer
ldrb w8, [x28]
ldrb w9, [x0]
cmp w8, w9
b.ne Lneg_long_footer
cmp x5, x16
b.lo Lneg_long
Lneg_long_footer:
cmp x5, x16
b.lo Lneg_main
b Lneg_done
Lneg_error:
mov x5, #0
Lneg_done:
str w1, [x19]
mov x0, x5
ldp x19, x20, [sp]
ldp x21, x22, [sp, #16]
ldp x23, x24, [sp, #32]
ldp x25, x26, [sp, #48]
ldp x27, x28, [sp, #64]
add sp, sp, #80
ret
