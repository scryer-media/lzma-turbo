// SDK 26.03, Asm/x86/LzFindOpt.asm, GetMatchesSpecN_2.
// Public domain, upstream commit 0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17.
// Complete System V macro expansion in LLVM Intel syntax. Preserve upstream
// register allocation, scalar match extension, child addressing and long copy.
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
jmp     .Lsdk_main_loop
.p2align 6
.Lsdk_fill_empty:
mov     qword ptr [r15], 0
inc     edx
inc     ebx
mov     dword ptr [r13 - 4], 0
cmp     r13, qword ptr [rsp + 8 * 3]
jae     .Lsdk_fin
cmp     r14, r10
je      .Lsdk_fin
.Lsdk_main_loop:
mov     edi, [r14]
test    edi, edi
je      .Lsdk_fin_error
add     r14, 4
inc     rcx
add     r13, 4
mov     ebp, edx
sub     ebp, edi
lea     r15, [r9 + 8 * rbx]
cmp     edx, esi
jb      .Lsdk_directmode
cmp     edi, esi
jae     .Lsdk_fill_empty
xor     eax, eax
mov     dword ptr [rsp + 8 * 1 + 0], ebx
sub     ebx, edi
cmovb   eax, esi
add     ebx, eax
jmp     .Lsdk_prepare_for_tree_loop
.Lsdk_directmode:
cmp     edi,  edx
je      .Lsdk_fill_empty
jae     .Lsdk_fin_error
mov     dword ptr [rsp + 8 * 1 + 0], ebx
mov     ebx, ebp
.Lsdk_prepare_for_tree_loop:
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
.Lsdk_tree_loop:
neg     rdi
mov     rsi, r10
cmp     r11, r10
cmovb   rsi, r11
add     rdi, rcx
mov     eax, [r9 + rbx * 8]
movzx   eax, byte ptr [rdi + 1 * rsi]
lea     rbx, [r9 + rbx * 8]
cmp     [rcx + 1 * rsi], al
je      .Lsdk_matched_1
jb      .Lsdk_left_0
mov     [r15], ebp
mov        ebp, [rbx + 4]
lea     r15, [rbx + 4]
sub     rdi, rcx
jmp     .Lsdk_next_node
.p2align 5
.Lsdk_left_0:
mov     [r14], ebp
mov        ebp, [rbx]
mov     r14, rbx
sub     rdi, rcx
.Lsdk_next_node:
mov     esi, dword ptr [rsp + 8 * 1 + 4]
add     edi, edx
cmp     ebp, edi
jae     .Lsdk_fin_error
dec     dword ptr [rsp + 8 * 0 + 4]
je      .Lsdk_finish_tree
mov     edi, edx
sub     edi, ebp
cmp     edx, esi
jae     .Lsdk_cyc_mode_2
mov     ebx, ebp
test    ebp, ebp
jne     .Lsdk_tree_loop
.Lsdk_finish_tree:
mov     dword ptr [r14], 0
mov     dword ptr [r15], 0
inc     edx
mov     rax, qword ptr [rsp + 8 * 5]
mov     rbx, r13
sub     rbx, rax
shr     ebx, 2
mov     [rax - 4], ebx
cmp     r13, qword ptr [rsp + 8 * 3]
jae     .Lsdk_fin
mov     ebx, dword ptr [rsp + 8 * 1 + 0]
mov     r14, qword ptr [rsp + 8 * 2]
mov     r10, qword ptr [rsp + 8 * 4]
inc     ebx
cmp     r14, r10
jne     .Lsdk_main_loop
jmp     .Lsdk_fin
.p2align 5
.Lsdk_cyc_mode_2:
cmp     edi, esi
jae     .Lsdk_finish_tree
mov     ebx, dword ptr [rsp + 8 * 1 + 0]
xor     eax, eax
sub     ebx, edi
cmovb   eax, esi
add     ebx, eax
jmp     .Lsdk_tree_loop
.p2align 5
.Lsdk_matched_1:
inc     rsi
je      .Lsdk_lenlimit_reach
movzx   eax, byte ptr [rdi + 1 * rsi]
cmp     [rcx + 1 * rsi], al
jne     .Lsdk_mismatch
.p2align 5
.Lsdk_match_loop:
inc     rsi
je      .Lsdk_lenlimit_reach
movzx   eax, byte ptr [rdi + 1 * rsi]
cmp     byte ptr [rcx + 1 * rsi], al
je      .Lsdk_match_loop
.Lsdk_mismatch:
jb      .Lsdk_left_2
mov     [r15], ebp
mov        ebp, [rbx + 4]
lea     r15, [rbx + 4]
mov     r11, rsi
jmp     .Lsdk_max_update
.p2align 5
.Lsdk_left_2:
mov     [r14], ebp
mov        ebp, [rbx]
mov     r14, rbx
mov     r10, rsi
.Lsdk_max_update:
sub     rdi, rcx
cmp     r12, rsi
jae     .Lsdk_next_node
mov     r12, rsi
add     rsi, r8
mov     [r13], esi
mov     eax, edi
not     eax
mov     [r13 + 4], eax
add     r13, 8
jmp     .Lsdk_next_node
.p2align 5
.Lsdk_lenlimit_reach:
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
je      .Lsdk_fin
cmp     r13, rbp
jae     .Lsdk_fin
cmp     r12d, [r14]
jne     .Lsdk_main_loop
movzx   eax, byte ptr [rdi]
cmp     [rcx], al
jne     .Lsdk_main_loop
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
.Lsdk_long_loop:
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
je      .Lsdk_long_footer
cmp     r12d, [r14]
jne     .Lsdk_long_footer
movzx   eax, byte ptr [rdi + 1 * rcx]
cmp     [rcx], al
jne     .Lsdk_long_footer
cmp     r13, rbp
jb      .Lsdk_long_loop
.Lsdk_long_footer:
sub     rbx, r9
shr     rbx, 3
add     edx, ebx
sub     edx, dword ptr [rsp + 8 * 1 + 0]
mov     esi, dword ptr [rsp + 8 * 1 + 4]
cmp     r13, rbp
jae     .Lsdk_fin
cmp     r14, r10
jne     .Lsdk_main_loop
jmp     .Lsdk_fin
.Lsdk_fin_error:
xor     r13, r13
.Lsdk_fin:
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
