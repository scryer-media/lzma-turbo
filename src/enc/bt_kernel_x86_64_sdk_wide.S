// SDK 26.03, Asm/x86/LzFindOpt.asm, GetMatchesSpecN_2.
// Public domain, upstream commit 0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17.
// Complete System V macro expansion in LLVM Intel syntax. Preserve upstream
// register allocation, child addressing and long copy. After the first two
// matching bytes, use the shipping bounded SSE2/word extension. This diagnostic
// changes no tree decisions, scheduler behavior, or core-dependent policy.
// The wrapper establishes A-F. Only two semantic additions are intentional:
// reject a zero head, and validate a descending child before decrementing cut.
// Both retain the checked Rust oracle's behavior on malformed tables.
// Native differential tests exercise this variant; Miri cannot execute it.
push rbx
push rbp
push r12
push r13
push r14
push r15
mov     rax, rsp
lea     rbx, [rax - 64]
and     rbx, -64
mov     rsp, rbx
mov     qword ptr [rsp + 8 * 7], rax
mov     r13,            r9
mov     dword ptr [rsp + 8 * 0], r8d
mov     r9,          rcx
mov     r8,           rdx
mov     edx,          esi
mov     rcx,           rdi
mov rbx,  [rax + 72]
mov qword ptr [rsp + 8 * 3], rbx
mov     r10, [rax + 80]
mov     qword ptr [rsp + 8 * 4], r10
mov     ebx, [rax + 88]
mov     r14, [rax + 64]
mov     esi, [rax + 96]
mov     dword ptr [rsp + 8 * 1 + 4], esi
sub     r8, rcx
neg     r8d
inc     r8d
mov     eax, [rax + 56]
sub     rax, r8
mov     qword ptr [rsp + 8 * 6], rax
jmp     .Lwide_main_loop
.p2align 6
.Lwide_fill_empty:
mov     qword ptr [r15], 0
inc     edx
inc     ebx
mov     dword ptr [r13 - 4], 0
cmp     r13, qword ptr [rsp + 8 * 3]
jae     .Lwide_fin
cmp     r14, r10
je      .Lwide_fin
.Lwide_main_loop:
mov     edi, [r14]
test    edi, edi
je      .Lwide_fin_error
add     r14, 4
inc     rcx
add     r13, 4
mov     ebp, edx
sub     ebp, edi
lea     r15, [r9 + 8 * rbx]
cmp     edx, esi
jb      .Lwide_directmode
cmp     edi, esi
jae     .Lwide_fill_empty
xor     eax, eax
mov     dword ptr [rsp + 8 * 1 + 0], ebx
sub     ebx, edi
cmovb   eax, esi
add     ebx, eax
jmp     .Lwide_prepare_for_tree_loop
.Lwide_directmode:
cmp     edi,  edx
je      .Lwide_fill_empty
jae     .Lwide_fin_error
mov     dword ptr [rsp + 8 * 1 + 0], ebx
mov     ebx, ebp
.Lwide_prepare_for_tree_loop:
mov     r10, r8
mov     qword ptr [rsp + 8 * 2], r14
lea     r14, [r15 + 4]
mov     qword ptr [rsp + 8 * 5], r13
neg     r10
mov     r11, r10
mov     eax, dword ptr [rsp + 8 * 0]
mov     r12, qword ptr [rsp + 8 * 6]
mov     dword ptr [rsp + 8 * 0 + 4], eax
.p2align 5
.Lwide_tree_loop:
neg     rdi
mov     rsi, r10
cmp     r11, r10
cmovb   rsi, r11
add     rdi, rcx
mov     eax, [r9 + rbx * 8]
movzx   eax, byte ptr [rdi + 1 * rsi]
lea     rbx, [r9 + rbx * 8]
cmp     [rcx + 1 * rsi], al
je      .Lwide_matched_1
jb      .Lwide_left_0
mov     [r15], ebp
mov        ebp, [rbx + 4]
lea     r15, [rbx + 4]
sub     rdi, rcx
jmp     .Lwide_next_node
.p2align 5
.Lwide_left_0:
mov     [r14], ebp
mov        ebp, [rbx]
mov     r14, rbx
sub     rdi, rcx
.Lwide_next_node:
mov     esi, dword ptr [rsp + 8 * 1 + 4]
add     edi, edx
cmp     ebp, edi
jae     .Lwide_fin_error
dec     dword ptr [rsp + 8 * 0 + 4]
je      .Lwide_finish_tree
mov     edi, edx
sub     edi, ebp
cmp     edx, esi
jae     .Lwide_cyc_mode_2
mov     ebx, ebp
test    ebp, ebp
jne     .Lwide_tree_loop
.Lwide_finish_tree:
mov     dword ptr [r14], 0
mov     dword ptr [r15], 0
inc     edx
mov     rax, qword ptr [rsp + 8 * 5]
mov     rbx, r13
sub     rbx, rax
shr     ebx, 2
mov     [rax - 4], ebx
cmp     r13, qword ptr [rsp + 8 * 3]
jae     .Lwide_fin
mov     ebx, dword ptr [rsp + 8 * 1 + 0]
mov     r14, qword ptr [rsp + 8 * 2]
mov     r10, qword ptr [rsp + 8 * 4]
inc     ebx
cmp     r14, r10
jne     .Lwide_main_loop
jmp     .Lwide_fin
.p2align 5
.Lwide_cyc_mode_2:
cmp     edi, esi
jae     .Lwide_finish_tree
mov     ebx, dword ptr [rsp + 8 * 1 + 0]
xor     eax, eax
sub     ebx, edi
cmovb   eax, esi
add     ebx, eax
jmp     .Lwide_tree_loop
.p2align 5
.Lwide_matched_1:
inc     rsi
je      .Lwide_lenlimit_reach
movzx   eax, byte ptr [rdi + 1 * rsi]
cmp     [rcx + 1 * rsi], al
jne     .Lwide_mismatch
.p2align 5
.Lwide_match_loop:
inc     rsi
je      .Lwide_lenlimit_reach
.Lwide_word_scan:
// SAFETY: the same negative-offset margin covers complete 16-byte reads.
cmp     rsi, -16
ja      .Lwide_word8_scan
movdqu  xmm0, XMMWORD PTR [rdi + rsi]
movdqu  xmm1, XMMWORD PTR [rcx + rsi]
pcmpeqb xmm0, xmm1
pmovmskb eax, xmm0
xor     eax, 65535
jnz     .Lwide_word16_mismatch
add     rsi, 16
je      .Lwide_lenlimit_reach
jmp     .Lwide_word_scan
.Lwide_word16_mismatch:
// The nonzero mask has one set bit per differing byte, in address order.
bsf     eax, eax
add     rsi, rax
jmp     .Lwide_byte_scan
.Lwide_word8_scan:
// SAFETY: rsi is a negative offset from the match limit. rsi <= -8
// keeps both complete words inside the window/history margins C, D and E.
cmp     rsi, -8
ja      .Lwide_byte_scan
mov     rax, QWORD PTR [rdi + rsi]
xor     rax, QWORD PTR [rcx + rsi]
jnz     .Lwide_word_mismatch
add     rsi, 8
je      .Lwide_lenlimit_reach
jmp     .Lwide_word_scan
.Lwide_word_mismatch:
// The XOR is nonzero. Its first set byte is the first mismatch on x86-64.
bsf     rax, rax
shr     eax, 3
add     rsi, rax
.Lwide_byte_scan:
movzx   eax, BYTE PTR [rdi + rsi]
cmp     BYTE PTR [rcx + rsi], al
je      .Lwide_match_loop
.Lwide_mismatch:
jb      .Lwide_left_2
mov     [r15], ebp
mov        ebp, [rbx + 4]
lea     r15, [rbx + 4]
mov     r11, rsi
jmp     .Lwide_max_update
.p2align 5
.Lwide_left_2:
mov     [r14], ebp
mov        ebp, [rbx]
mov     r14, rbx
mov     r10, rsi
.Lwide_max_update:
sub     rdi, rcx
cmp     r12, rsi
jae     .Lwide_next_node
mov     r12, rsi
add     rsi, r8
mov     [r13], esi
mov     eax, edi
not     eax
mov     [r13 + 4], eax
add     r13, 8
jmp     .Lwide_next_node
.p2align 5
.Lwide_lenlimit_reach:
mov     r12, rcx
sub     r12, rdi
lea     r11, [r12 - 1]
mov     eax, [rbx]
mov     [r15], eax
mov     eax, [rbx + 4]
mov     [r14], eax
mov     [r13], r8d
mov     [r13 + 4], r11d
add     r13, 8
mov     rax, qword ptr [rsp + 8 * 5]
mov     rbx, r13
sub     rbx, rax
shr     ebx, 2
mov     [rax - 4], ebx
mov     r14, qword ptr [rsp + 8 * 2]
mov     r10, qword ptr [rsp + 8 * 4]
inc     edx
mov     ebx, dword ptr [rsp + 8 * 1 + 0]
inc     ebx
mov     rbp, qword ptr [rsp + 8 * 3]
mov     esi, dword ptr [rsp + 8 * 1 + 4]
cmp     r14, r10
je      .Lwide_fin
cmp     r13, rbp
jae     .Lwide_fin
cmp     r12d, [r14]
jne     .Lwide_main_loop
movzx   eax, byte ptr [rdi]
cmp     [rcx], al
jne     .Lwide_main_loop
mov     dword ptr [rsp + 8 * 1 + 0], ebx
shl     rsi, 3
sub     rdi, rcx
xor     eax, eax
cmp     ebx, r12d
lea     rbx, [r9 + 8 * rbx]
lea     r15, [rbx + 8 * rdi]
cmovb   rax, rsi
add     r15, rax
add     rsi, r9
.p2align 5
.Lwide_long_loop:
add     r14, 4
mov     rax, [r15]
add     r15, 8
mov     [rbx], rax
add     rbx, 8
cmp     r15, rsi
cmove   r15, r9
mov     dword ptr [r13], 2
mov     [r13 + 4], r8d
mov     [r13 + 8], r11d
add     r13, 12
inc     rcx
cmp     r14, r10
je      .Lwide_long_footer
cmp     r12d, [r14]
jne     .Lwide_long_footer
movzx   eax, byte ptr [rdi + 1 * rcx]
cmp     [rcx], al
jne     .Lwide_long_footer
cmp     r13, rbp
jb      .Lwide_long_loop
.Lwide_long_footer:
sub     rbx, r9
shr     rbx, 3
add     edx, ebx
sub     edx, dword ptr [rsp + 8 * 1 + 0]
mov     esi, dword ptr [rsp + 8 * 1 + 4]
cmp     r13, rbp
jae     .Lwide_fin
cmp     r14, r10
jne     .Lwide_main_loop
jmp     .Lwide_fin
.Lwide_fin_error:
xor     r13, r13
.Lwide_fin:
mov     rsp, qword ptr [rsp + 8 * 7]
mov     rax, [rsp + 104]
mov     [rax], edx
mov     rax, r13
pop r15
pop r14
pop r13
pop r12
pop rbp
pop rbx
ret
