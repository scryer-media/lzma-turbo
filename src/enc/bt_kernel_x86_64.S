// SDK 26.03, Asm/x86/LzFindOpt.asm, GetMatchesSpecN_2.
// Public domain, upstream commit 0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17.
// MASM macros expanded for the System V x86-64 ABI and LLVM Intel syntax.
// The Rust wrapper proves the input, tree and output margins. Two deliberate
// differences retain the checked kernel's behavior on invalid tables: reject
// a zero head, and reject a non-descending child before testing the cut count.
// Negative lengths, load order and long-match copying follow the upstream
// assembly. Keep the cut counter in r12d and compare each child directly with
// its saved parent; the best length changes only when a match is emitted.
// r8 holds the invariant input limit minus logical position during a walk.
// The constant match length lives in caller-clobbered xmm3 and returns to r8
// for long-match copying; both cursors advance together before base_main.
// This keeps child-address formation in general registers.
// After two equal bytes, the length extension
// uses bounded 16/8-byte comparisons and a scalar tail. SSE2 is baseline x86-64.
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
mov     qword ptr [rsp + 56], rax
mov     r13,            r9
mov     dword ptr [rsp + 0], r8d
mov     r9,          rcx
mov     r8,           rdx
mov     edx,          esi
mov     rcx,           rdi
mov rbx, [rax + 72]
mov qword ptr [rsp + 24], rbx
mov     r10, [rax + 80]
mov     qword ptr [rsp + 32], r10
mov     ebx, [rax + 88]
mov     r14, [rax + 64]
mov     esi, [rax + 96]
mov     dword ptr [rsp + 12], esi
sub     r8, rcx
neg     r8d
inc     r8d
mov     eax, [rax + 56]
sub     rax, r8
mov     qword ptr [rsp + 48], rax
movq    xmm3, r8
Lbt_base_main:
mov     r8, rcx
sub     r8, rdx
inc     r8
jmp     Lbt_main_loop
.p2align 6
Lbt_fill_empty:
mov     QWORD PTR [r15], 0
inc     edx
inc     ebx
mov     DWORD PTR [r13 - 4], 0
cmp     r13, qword ptr [rsp + 24]
jae     Lbt_fin
cmp     r14, r10
je      Lbt_fin
Lbt_main_loop:
mov     edi, [r14]
test edi, edi
je Lbt_fin_error
add     r14, 4
inc     rcx
add     r13, 4
mov     ebp, edx
sub     ebp, edi
lea     r15, [r9 + 8 * rbx]
cmp     edx, esi
jb      Lbt_directMode
cmp     edi, esi
jae     Lbt_fill_empty
xor     eax, eax
mov     dword ptr [rsp + 8], ebx
sub     ebx, edi
cmovb   eax, esi
add     ebx, eax
jmp     Lbt_prepare_for_tree_loop
Lbt_directMode:
cmp     edi,  edx
je      Lbt_fill_empty
jae     Lbt_fin_error
mov     dword ptr [rsp + 8], ebx
mov     ebx, ebp
Lbt_prepare_for_tree_loop:
movq    r10, xmm3
mov     qword ptr [rsp + 16], r14
lea     r14, [r15 + 4]
mov     qword ptr [rsp + 40], r13
neg     r10
mov     r11, r10
mov     r12d, dword ptr [rsp + 0]
mov     eax, dword ptr [rsp + 48]
mov     dword ptr [rsp + 4], eax
.p2align 5
Lbt_tree_loop:
lea     rdi, [r8 + rbp]
mov     rsi, r10
cmp     r11, r10
cmovb   rsi, r11
mov     eax, [r9 + rbx * 8]
movzx   eax, BYTE PTR [rdi + 1 * rsi]
lea     rbx, [r9 + rbx * 8]
cmp     [rcx + 1 * rsi], al
je      Lbt_matched_1
jb      Lbt_left_0
mov     [r15], ebp
mov     eax, ebp
mov        ebp, [rbx + 4]
lea     r15, [rbx + 4]
jmp     Lbt_next_node
.p2align 5
Lbt_left_0:
mov     [r14], ebp
mov     eax, ebp
mov        ebp, [rbx]
mov     r14, rbx
Lbt_next_node:
mov     esi, dword ptr [rsp + 12]
cmp     ebp, eax
jae     Lbt_fin_error
dec     r12d
je      Lbt_finish_tree
mov     edi, edx
sub     edi, ebp
cmp     edx, esi
jae     Lbt_cyc_mode_2
mov     ebx, ebp
test    ebp, ebp
jne     Lbt_tree_loop
Lbt_finish_tree:
mov     DWORD PTR [r14], 0
mov     DWORD PTR [r15], 0
inc     edx
mov     rax, qword ptr [rsp + 40]
mov     rbx, r13
sub     rbx, rax
shr     ebx, 2
mov     [rax - 4], ebx
cmp     r13, qword ptr [rsp + 24]
jae     Lbt_fin
mov     ebx, dword ptr [rsp + 8]
mov     r14, qword ptr [rsp + 16]
mov     r10, qword ptr [rsp + 32]
inc     ebx
cmp     r14, r10
jne     Lbt_main_loop
jmp     Lbt_fin
.p2align 5
Lbt_cyc_mode_2:
cmp     edi, esi
jae     Lbt_finish_tree
mov     ebx, dword ptr [rsp + 8]
xor     eax, eax
sub     ebx, edi
cmovb   eax, esi
add     ebx, eax
jmp     Lbt_tree_loop
.p2align 5
Lbt_matched_1:
inc     rsi
je      Lbt_lenLimit_reach
movzx   eax, BYTE PTR [rdi + 1 * rsi]
cmp     [rcx + 1 * rsi], al
jne     Lbt_mismatch
.p2align 5
Lbt_match_loop:
inc     rsi
je      Lbt_lenLimit_reach
Lbt_word_scan:
// SAFETY: the same negative-offset margin covers complete 16-byte reads.
cmp     rsi, -16
ja      Lbt_word8_scan
movdqu  xmm0, XMMWORD PTR [rdi + rsi]
movdqu  xmm1, XMMWORD PTR [rcx + rsi]
pcmpeqb xmm0, xmm1
pmovmskb eax, xmm0
xor     eax, 65535
jnz     Lbt_word16_mismatch
add     rsi, 16
je      Lbt_lenLimit_reach
jmp     Lbt_word_scan
Lbt_word16_mismatch:
// The nonzero mask has one set bit per differing byte, in address order.
bsf     eax, eax
add     rsi, rax
jmp     Lbt_byte_scan
Lbt_word8_scan:
// SAFETY: rsi is a negative offset from the match limit. rsi <= -8
// keeps both complete words inside the window/history margins C, D and E.
cmp     rsi, -8
ja      Lbt_byte_scan
mov     rax, QWORD PTR [rdi + rsi]
xor     rax, QWORD PTR [rcx + rsi]
jnz     Lbt_word_mismatch
add     rsi, 8
je      Lbt_lenLimit_reach
jmp     Lbt_word_scan
Lbt_word_mismatch:
// The XOR is nonzero. Its first set byte is the first mismatch on x86-64.
bsf     rax, rax
shr     eax, 3
add     rsi, rax
Lbt_byte_scan:
movzx   eax, BYTE PTR [rdi + rsi]
cmp     BYTE PTR [rcx + rsi], al
je      Lbt_match_loop
Lbt_mismatch:
jb      Lbt_left_2
mov     [r15], ebp
mov     eax, ebp
mov        ebp, [rbx + 4]
lea     r15, [rbx + 4]
mov     r11, rsi
jmp     Lbt_max_update
.p2align 5
Lbt_left_2:
mov     [r14], ebp
mov     eax, ebp
mov        ebp, [rbx]
mov     r14, rbx
mov     r10, rsi
Lbt_max_update:
cmp     dword ptr [rsp + 4], esi
jae     Lbt_next_node
mov     dword ptr [rsp + 4], esi
movq    rdi, xmm3
add     rsi, rdi
mov     [r13], esi
mov     edi, edx
sub     edi, eax
dec     edi
mov     [r13 + 4], edi
add     r13, 8
jmp     Lbt_next_node
.p2align 5
Lbt_lenLimit_reach:
movq    r8, xmm3
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
mov     rax, qword ptr [rsp + 40]
mov     rbx, r13
sub     rbx, rax
shr     ebx, 2
mov     [rax - 4], ebx
mov     r14, qword ptr [rsp + 16]
mov     r10, qword ptr [rsp + 32]
inc     edx
mov     ebx, dword ptr [rsp + 8]
inc     ebx
mov     rbp, qword ptr [rsp + 24]
mov     esi, dword ptr [rsp + 12]
cmp     r14, r10
je      Lbt_fin
cmp     r13, rbp
jae     Lbt_fin
cmp     r12d, [r14]
jne     Lbt_base_main
movzx   eax, BYTE PTR [rdi]
cmp     [rcx], al
jne     Lbt_base_main
mov     dword ptr [rsp + 8], ebx
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
Lbt_long_loop:
add     r14, 4
mov     rax, [r15]
add     r15, 8
mov     [rbx], rax
add     rbx, 8
cmp     r15, rsi
cmove   r15, r9
mov     DWORD PTR [r13], 2
mov     [r13 + 4], r8d
mov     [r13 + 8], r11d
add     r13, 12
inc     rcx
cmp     r14, r10
je      Lbt_long_footer
cmp     r12d, [r14]
jne     Lbt_long_footer
movzx   eax, BYTE PTR [rdi + 1 * rcx]
cmp     [rcx], al
jne     Lbt_long_footer
cmp     r13, rbp
jb      Lbt_long_loop
Lbt_long_footer:
sub     rbx, r9
shr     rbx, 3
add     edx, ebx
sub     edx, dword ptr [rsp + 8]
mov     esi, dword ptr [rsp + 12]
cmp     r13, rbp
jae     Lbt_fin
cmp     r14, r10
jne     Lbt_base_main
jmp     Lbt_fin
Lbt_fin_error:
xor     r13, r13
Lbt_fin:
mov     rsp, qword ptr [rsp + 56]
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
