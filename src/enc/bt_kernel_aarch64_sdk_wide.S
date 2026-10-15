// SDK 26.03 GetMatchesSpecN_2, C/LzFindOpt.c at
// 0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17.
// SDK ARM64 control flow with the portable bounded eight-byte match scan.
// Full words are loaded only when all eight bytes are below the inclusive
// limit; rbit/clz locates the first differing byte, followed by a byte tail.
// The original instruction sequence remains in bt_kernel_aarch64_sdk.S.
// Diagnostic variant: checked entry margins remain in bt_kernel.rs.
// x0..x7: lenLimit, pos, cur, son, cut, output, maxLen, heads.
// Incoming stack: output limit, heads end, cyclic position, cyclic size,
// position result. The 128-byte frame saves x19..x30; no calls or x18 use.
// Every branch target is local. No SIMD callee-saved register is modified.
    sub	sp, sp, #0x80
    stp	x28, x27, [sp, #0x20]
    stp	x26, x25, [sp, #0x30]
    stp	x24, x23, [sp, #0x40]
    stp	x22, x21, [sp, #0x50]
    stp	x20, x19, [sp, #0x60]
    stp	x29, x30, [sp, #0x70]
    mov	x14, x5
    mov	x24, x4
    mov	x9, x2
    ldr	x8, [sp, #0xa0]
    str	x8, [sp, #0x8]
    ldr	w10, [sp, #0x98]
    ldp	x13, x11, [sp, #0x88]
    add	x8, x3, #0x8
    str	x8, [sp]
    ldr	x4, [sp, #0x80]
    str	x6, [sp, #0x18]
    str	w24, [sp, #0x14]
Lsdk_arm64_wide_4c:
    cmp	x7, x13
    b.eq	Lsdk_arm64_wide_344
    mov	x16, x7
    ldr	w15, [x16], #0x4
    cbz	w15, Lsdk_arm64_wide_31c
    mov	x20, x10
    cmp	w10, w1
    b.ls	Lsdk_arm64_wide_78
    mov	x20, x1
    cmp	w15, w1
    b.hi	Lsdk_arm64_wide_31c
Lsdk_arm64_wide_78:
    add	x17, x0, #0x1
    cmp	w15, w20
    b.hs	Lsdk_arm64_wide_1d4
    add	x23, x3, x11, lsl #3
    add	x22, x14, #0x4
    mov	w19, w20
    add	x30, x23, #0x4
    add	x26, x9, x6
    mov	x12, x9
    mov	x27, x9
    mov	x5, x22
Lsdk_arm64_wide_a4:
    mov	w21, w15
    neg	x15, x21
    subs	x8, x11, x21
    csel	x2, x19, xzr, lo
    add	x8, x8, x2
    add	x28, x3, x8, lsl #3
    ldr w8, [x28] // Early child fetch, as in the portable register walk.
    cmp	x12, x27
    csel	x2, x12, x27, lo
    sub	x8, x2, x21
    ldrb	w8, [x8]
    ldrb	w25, [x2]
    cmp	w8, w25
    b.ne	Lsdk_arm64_wide_154
    add x25, x2, #1
    cmp x2, x0
    b.eq Lsdk_arm64_wide_11c
Lsdk_arm64_wide_scan_word:
    sub x8, x0, x25
    cmp x8, #7
    b.lo Lsdk_arm64_wide_scan_tail
    ldr x8, [x25, x15]
    ldr x6, [x25]
    eor x8, x8, x6
    cbnz x8, Lsdk_arm64_wide_scan_diff
    add x25, x25, #8
    cmp x25, x0
    b.ls Lsdk_arm64_wide_scan_word
    b Lsdk_arm64_wide_11c
Lsdk_arm64_wide_scan_diff:
    rbit x8, x8
    clz x8, x8
    lsr x8, x8, #3
    add x25, x25, x8
    b Lsdk_arm64_wide_11c
Lsdk_arm64_wide_scan_tail:
    ldrb w8, [x25, x15]
    ldrb w6, [x25]
    cmp w8, w6
    b.ne Lsdk_arm64_wide_11c
    add x25, x25, #1
    cmp x25, x0
    b.ls Lsdk_arm64_wide_scan_tail
Lsdk_arm64_wide_11c:
    cmp	x26, x25
    b.hs	Lsdk_arm64_wide_150
    ldr	w2, [x28]
    sub	w6, w25, w9
    sub	w26, w21, #0x1
    add	x8, x5, #0x8
    stp	w6, w26, [x5]
    cmp	x25, x17
    b.eq	Lsdk_arm64_wide_208
    mov	x26, x25
    mov	x2, x25
    mov	x5, x8
    b	Lsdk_arm64_wide_154
Lsdk_arm64_wide_150:
    mov	x2, x25
Lsdk_arm64_wide_154:
    sub	w21, w1, w21
    ldrb	w8, [x2, x15]
    ldrb	w15, [x2]
    cmp	w8, w15
    b.hs	Lsdk_arm64_wide_188
    ldr	w8, [x28, #0x4]!
    str	w21, [x23]
    cmp	w8, w21
    b.hs	Lsdk_arm64_wide_31c
    mov	x23, x28
    mov	x28, x30
    mov	x27, x2
    b	Lsdk_arm64_wide_19c
Lsdk_arm64_wide_188:
    ldr	w8, [x28]
    str	w21, [x30]
    mov	x12, x2
    cmp	w8, w21
    b.hs	Lsdk_arm64_wide_31c
Lsdk_arm64_wide_19c:
    subs	w24, w24, #0x1
    b.eq	Lsdk_arm64_wide_1b4
    sub	w15, w1, w8
    cmp	w15, w20
    mov	x30, x28
    b.lo	Lsdk_arm64_wide_a4
Lsdk_arm64_wide_1b4:
    str	wzr, [x23]
    str	wzr, [x28]
    sub	x8, x5, x22
    lsr	x8, x8, #2
    str	w8, [x14]
Lsdk_arm64_wide_1c8:
    ldr	x6, [sp, #0x18]
    ldr	w24, [sp, #0x14]
    b	Lsdk_arm64_wide_1e4
Lsdk_arm64_wide_1d4:
    str	wzr, [x14], #0x4
    add	x8, x3, x11, lsl #3
    str	xzr, [x8]
    mov	x5, x14
Lsdk_arm64_wide_1e4:
    add	x1, x1, #0x1
    add	x11, x11, #0x1
    add	x9, x9, #0x1
    mov	x7, x16
    mov	x14, x5
    mov	x0, x17
    cmp	x5, x4
    b.lo	Lsdk_arm64_wide_4c
    b	Lsdk_arm64_wide_348
Lsdk_arm64_wide_208:
    ldr	w12, [x28, #0x4]
    str	w2, [x23]
    str	w12, [x30]
    sub	x12, x8, x22
    lsr	x12, x12, #2
    str	w12, [x14]
    cmp	x16, x13
    b.eq	Lsdk_arm64_wide_314
    ldr	w12, [x16]
    cmp	w12, w21
    b.ne	Lsdk_arm64_wide_314
    ldrb	w12, [x17, x15]
    ldrb	w14, [x17]
    cmp	w12, w14
    b.ne	Lsdk_arm64_wide_314
    cmp	x8, x4
    b.hs	Lsdk_arm64_wide_314
    mov	x14, #0x0
    sub	w8, w0, w9
    add	w15, w8, #0x1
    add	x17, x11, #0x1
    lsl	x8, x11, #3
    ldr	x12, [sp]
    add	x8, x12, x8
    add	x2, x7, #0x8
    add	x23, x5, #0x14
    add	x7, x0, #0x2
    sub	x22, x7, x21
    mov	x24, x2
    sub	x12, x8, x21, lsl #3
Lsdk_arm64_wide_280:
    mov	x20, x14
    mov	x5, x23
    mov	x14, x23
    mov	w16, #0x2
    stp	w16, w15, [x14, #-0xc]!
    mov	x16, x24
    str	w26, [x14, #0x8]
    add	x6, x17, x20
    cmp	x6, x21
    csel	x6, x19, xzr, lo
    add	x6, x12, x6, lsl #3
    ldr	d0, [x6, x20, lsl #3]
    str	d0, [x8, x20, lsl #3]
    cmp	x24, x13
    b.eq	Lsdk_arm64_wide_2f0
    ldr	w6, [x2, x20, lsl #2]
    cmp	w6, w21
    b.ne	Lsdk_arm64_wide_2f0
    ldrb	w6, [x22, x20]
    ldrb	w23, [x7, x20]
    cmp	w6, w23
    b.ne	Lsdk_arm64_wide_2f0
    add	x6, x14, #0xc
    add	x14, x20, #0x1
    add	x24, x16, #0x4
    add	x23, x5, #0xc
    cmp	x6, x4
    b.lo	Lsdk_arm64_wide_280
Lsdk_arm64_wide_2f0:
    add	x8, x11, x20
    add	x11, x8, #0x1
    add	x8, x9, x20
    add	x9, x8, #0x1
    add	x8, x1, x20
    add	x1, x8, #0x1
    add	x8, x0, x20
    add	x17, x8, #0x2
    b	Lsdk_arm64_wide_1c8
Lsdk_arm64_wide_314:
    mov	x5, x8
    b	Lsdk_arm64_wide_1c8
Lsdk_arm64_wide_31c:
    mov	x5, #0x0
Lsdk_arm64_wide_320:
    mov	x0, x5
    ldp	x29, x30, [sp, #0x70]
    ldp	x20, x19, [sp, #0x60]
    ldp	x22, x21, [sp, #0x50]
    ldp	x24, x23, [sp, #0x40]
    ldp	x26, x25, [sp, #0x30]
    ldp	x28, x27, [sp, #0x20]
    add	sp, sp, #0x80
    ret
Lsdk_arm64_wide_344:
    mov	x5, x14
Lsdk_arm64_wide_348:
    ldr	x8, [sp, #0x8]
    str	w1, [x8]
    b	Lsdk_arm64_wide_320
