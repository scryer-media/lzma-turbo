//! C: `MatchFinder_Normalize3` and the `LzFind_SaturSub` kernel family.

/// C: `MatchFinder_Normalize3`: scalar prefix to 128-byte alignment,
/// whole 128-byte blocks, then the scalar tail. Empty hash entries stay zero.
pub(crate) fn normalize3(sub_value: u32, items: &mut [u32]) {
    let prefix = ((128 - (items.as_ptr().addr() & 127)) & 127) / 4;
    let (head, rest) = items.split_at_mut(prefix.min(items.len()));
    scalar(sub_value, head);
    let bulk_len = rest.len() & !31;
    let (bulk, tail) = rest.split_at_mut(bulk_len);
    if !bulk.is_empty() {
        dispatch(sub_value, bulk);
    }
    scalar(sub_value, tail);
}

fn scalar(sub_value: u32, items: &mut [u32]) {
    for value in items {
        *value = value.saturating_sub(sub_value);
    }
}

fn dispatch(sub_value: u32, items: &mut [u32]) {
    #[cfg(all(not(miri), any(target_arch = "x86", target_arch = "x86_64")))]
    {
        #[cfg(feature = "std")]
        let (avx2, sse41) = (
            std::is_x86_feature_detected!("avx2"),
            std::is_x86_feature_detected!("sse4.1"),
        );
        #[cfg(not(feature = "std"))]
        let (avx2, sse41) = (
            cfg!(target_feature = "avx2"),
            cfg!(target_feature = "sse4.1"),
        );
        if avx2 {
            // SAFETY: feature detection above, and normalize3 supplies aligned,
            // nonempty whole 128-byte blocks within the exclusive slice.
            unsafe { x86::avx2(sub_value, items) };
            return;
        }
        if sse41 {
            // SAFETY: same slice invariant, with SSE4.1 checked above.
            unsafe { x86::sse41(sub_value, items) };
            return;
        }
    }
    #[cfg(all(not(miri), target_arch = "aarch64", target_feature = "neon"))]
    {
        // SAFETY: NEON is enabled for this target; the aligned slice contains
        // a nonempty whole number of 128-byte blocks.
        unsafe { neon(sub_value, items) };
        return;
    }
    #[allow(unreachable_code)]
    scalar(sub_value, items);
}

#[cfg(all(not(miri), any(target_arch = "x86", target_arch = "x86_64")))]
mod x86 {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    /// C: `LzFind_SaturSub_128`, four 128-bit operations per iteration.
    #[target_feature(enable = "sse4.1")]
    pub(super) unsafe fn sse41(sub_value: u32, items: &mut [u32]) {
        let sub = _mm_set1_epi32(sub_value as i32);
        for block in items.chunks_exact_mut(16) {
            for offset in [0, 4, 8, 12] {
                // SAFETY: caller supplies 128-byte alignment and whole blocks;
                // offsets select aligned, disjoint four-word lanes in this block.
                unsafe {
                    let p = block.as_mut_ptr().add(offset).cast();
                    let value = _mm_load_si128(p);
                    _mm_store_si128(p, _mm_sub_epi32(_mm_max_epu32(value, sub), sub));
                }
            }
        }
    }

    /// C: `LzFind_SaturSub_256`, four 256-bit operations per iteration.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn avx2(sub_value: u32, items: &mut [u32]) {
        let sub = _mm256_set1_epi32(sub_value as i32);
        for block in items.chunks_exact_mut(32) {
            for offset in [0, 8, 16, 24] {
                // SAFETY: caller supplies 128-byte alignment and whole blocks;
                // each offset selects eight initialized words inside this block.
                unsafe {
                    let p = block.as_mut_ptr().add(offset).cast();
                    let value = _mm256_load_si256(p);
                    _mm256_store_si256(p, _mm256_sub_epi32(_mm256_max_epu32(value, sub), sub));
                }
            }
        }
    }
}

/// C: ARM64 `LzFind_SaturSub_128`.
#[cfg(all(not(miri), target_arch = "aarch64", target_feature = "neon"))]
#[target_feature(enable = "neon")]
unsafe fn neon(sub_value: u32, items: &mut [u32]) {
    use core::arch::aarch64::*;
    let sub = vdupq_n_u32(sub_value);
    for block in items.chunks_exact_mut(16) {
        for offset in [0, 4, 8, 12] {
            // SAFETY: each four-word lane is initialized and inside the exclusive
            // block; the caller checked the NEON target feature.
            unsafe {
                let p = block.as_mut_ptr().add(offset);
                vst1q_u32(p, vsubq_u32(vmaxq_u32(vld1q_u32(p), sub), sub));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(128))]
    struct Aligned([u32; 384]);

    #[test]
    fn normalization_matches_scalar_at_every_alignment_and_tail() {
        for sub in [0, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_fffe, u32::MAX] {
            for offset in 0..32 {
                for len in [0, 1, 3, 15, 16, 31, 32, 33, 63, 64, 65, 127, 128, 257] {
                    let mut actual = Aligned(core::array::from_fn(|i| {
                        [
                            0,
                            sub.wrapping_sub(1),
                            sub,
                            sub.wrapping_add(1),
                            u32::MAX,
                            (i as u32).wrapping_mul(0x9e37_79b9),
                        ][i % 6]
                    }));
                    let mut expected = actual.0;
                    scalar(sub, &mut expected[offset..offset + len]);
                    normalize3(sub, &mut actual.0[offset..offset + len]);
                    assert_eq!(actual.0, expected);
                }
            }
        }
    }

    #[cfg(all(
        feature = "std",
        not(miri),
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    #[test]
    fn normalization_x86_kernels_match_scalar() {
        for sub in [0, 1, 0x8000_0000, u32::MAX] {
            for len in [32, 64, 128, 384] {
                let original = core::array::from_fn(|i| (i as u32).wrapping_mul(0x9e37_79b9));
                let mut expected = original;
                scalar(sub, &mut expected[..len]);
                if std::is_x86_feature_detected!("sse4.1") {
                    let mut actual = Aligned(original);
                    // SAFETY: runtime feature checked; aligned complete blocks.
                    unsafe { x86::sse41(sub, &mut actual.0[..len]) };
                    assert_eq!(actual.0, expected);
                }
                if std::is_x86_feature_detected!("avx2") {
                    let mut actual = Aligned(original);
                    // SAFETY: runtime feature checked; aligned complete blocks.
                    unsafe { x86::avx2(sub, &mut actual.0[..len]) };
                    assert_eq!(actual.0, expected);
                }
            }
        }
    }
}
