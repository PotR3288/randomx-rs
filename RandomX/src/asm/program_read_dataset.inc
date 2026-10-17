	mov ecx, ebp                       ;# ecx = ma
	and ecx, RANDOMX_DATASET_BASE_MASK
	xor r8, qword ptr [rdi+rcx]
	ror rbp, 32                        ;# swap "ma" and "mx"
	xor rbp, rax                       ;# modify "mx"
	mov edx, ebp                       ;# edx = mx
	and edx, RANDOMX_DATASET_BASE_MASK
#if defined(RANDOMX_PREFETCH_T1)
	prefetcht1 byte ptr [rdi+rdx]      ;# A/B experiment: T1 hint class (see build.rs RANDOMX_DATASET_PREFETCH)
#elif defined(RANDOMX_PREFETCH_T2)
	prefetcht2 byte ptr [rdi+rdx]      ;# A/B experiment: T2 hint class (see build.rs RANDOMX_DATASET_PREFETCH)
#else
	prefetchnta byte ptr [rdi+rdx]     ;# default: non-temporal, keeps dataset lines out of L1/L2
#endif
	xor r9,  qword ptr [rdi+rcx+8]
	xor r10, qword ptr [rdi+rcx+16]
	xor r11, qword ptr [rdi+rcx+24]
	xor r12, qword ptr [rdi+rcx+32]
	xor r13, qword ptr [rdi+rcx+40]
	xor r14, qword ptr [rdi+rcx+48]
	xor r15, qword ptr [rdi+rcx+56]
	