// Rolling hash implementations matching xdelta3 exactly.
//
// Two hash families:
//   - **Small checksum**: 4-byte window, multiply by LCG constant.
//     Used for target-to-target (self) copy matching.
//   - **Large checksum**: Adler-style rolling checksum with permuted input
//     bytes (historical xdelta3 behavior used by the C binding baseline).
//     Used for source-to-target copy matching.
//
// SIMD-accelerated forward match comparison using platform intrinsics
// (AVX2 on x86_64, NEON on aarch64, scalar fallback everywhere else).

// ---------------------------------------------------------------------------
// Constants matching xdelta3
// ---------------------------------------------------------------------------

/// LCG multiplier for 32-bit hashes (xd3_hash_multiplier32).
pub const HASH_MULT_32: u32 = 1_597_334_677;

/// Offset added to stored positions so 0 means "empty bucket".
pub const HASH_CKOFFSET: u64 = 1;

/// Function pointer type for byte-wise match scanning routines.
pub type MatchFn = fn(&[u8], &[u8], usize) -> usize;

/// Function pointer type for run-length scanning routines.
pub type RunLengthFn = fn(&[u8], u8, usize) -> usize;

/// Permutation table used by xdelta3's large checksum (`HASH_PERMUTE=1`).
const SINGLE_HASH: [u16; 256] = [
    0xbcd1, 0xbb65, 0x42c2, 0xdffe, 0x9666, 0x431b, 0x8504, 0xeb46, 0x6379, 0xd460, 0xcf14, 0x53cf,
    0xdb51, 0xdb08, 0x12c8, 0xf602, 0xe766, 0x2394, 0x250d, 0xdcbb, 0xa678, 0x02af, 0xa5c6, 0x7ea6,
    0xb645, 0xcb4d, 0xc44b, 0xe5dc, 0x9fe6, 0x5b5c, 0x35f5, 0x701a, 0x220f, 0x6c38, 0x1a56, 0x4ca3,
    0xffc6, 0xb152, 0x8d61, 0x7a58, 0x9025, 0x8b3d, 0xbf0f, 0x95a3, 0xe5f4, 0xc127, 0x3bed, 0x320b,
    0xb7f3, 0x6054, 0x333c, 0xd383, 0x8154, 0x5242, 0x4e0d, 0x0a94, 0x7028, 0x8689, 0x3a22, 0x0980,
    0x1847, 0xb0f1, 0x9b5c, 0x4176, 0xb858, 0xd542, 0x1f6c, 0x2497, 0x6a5a, 0x9fa9, 0x8c5a, 0x7743,
    0xa8a9, 0x9a02, 0x4918, 0x438c, 0xc388, 0x9e2b, 0x4cad, 0x01b6, 0xab19, 0xf777, 0x365f, 0x1eb2,
    0x091e, 0x7bf8, 0x7a8e, 0x5227, 0xeab1, 0x2074, 0x4523, 0xe781, 0x01a3, 0x163d, 0x3b2e, 0x287d,
    0x5e7f, 0xa063, 0xb134, 0x8fae, 0x5e8e, 0xb7b7, 0x4548, 0x1f5a, 0xfa56, 0x7a24, 0x900f, 0x42dc,
    0xcc69, 0x02a0, 0x0b22, 0xdb31, 0x71fe, 0x0c7d, 0x1732, 0x1159, 0xcb09, 0xe1d2, 0x1351, 0x52e9,
    0xf536, 0x5a4f, 0xc316, 0x6bf9, 0x8994, 0xb774, 0x5f3e, 0xf6d6, 0x3a61, 0xf82c, 0xcc22, 0x9d06,
    0x299c, 0x09e5, 0x1eec, 0x514f, 0x8d53, 0xa650, 0x5c6e, 0xc577, 0x7958, 0x71ac, 0x8916, 0x9b4f,
    0x2c09, 0x5211, 0xf6d8, 0xcaaa, 0xf7ef, 0x287f, 0x7a94, 0xab49, 0xfa2c, 0x7222, 0xe457, 0xd71a,
    0x00c3, 0x1a76, 0xe98c, 0xc037, 0x8208, 0x5c2d, 0xdfda, 0xe5f5, 0x0b45, 0x15ce, 0x8a7e, 0xfcad,
    0xaa2d, 0x4b5c, 0xd42e, 0xb251, 0x907e, 0x9a47, 0xc9a6, 0xd93f, 0x085e, 0x35ce, 0xa153, 0x7e7b,
    0x9f0b, 0x25aa, 0x5d9f, 0xc04d, 0x8a0e, 0x2875, 0x4a1c, 0x295f, 0x1393, 0xf760, 0x9178, 0x0f5b,
    0xfa7d, 0x83b4, 0x2082, 0x721d, 0x6462, 0x0368, 0x67e2, 0x8624, 0x194d, 0x22f6, 0x78fb, 0x6791,
    0xb238, 0xb332, 0x7276, 0xf272, 0x47ec, 0x4504, 0xa961, 0x9fc8, 0x3fdc, 0xb413, 0x007a, 0x0806,
    0x7458, 0x95c6, 0xccaa, 0x18d6, 0xe2ae, 0x1b06, 0xf3f6, 0x5050, 0xc8e8, 0xf4ac, 0xc04c, 0xf41c,
    0x992f, 0xae44, 0x5f1b, 0x1113, 0x1738, 0xd9a8, 0x19ea, 0x2d33, 0x9698, 0x2fe9, 0x323f, 0xcde2,
    0x6d71, 0xe37d, 0xb697, 0x2c4f, 0x4373, 0x9102, 0x075d, 0x8e25, 0x1672, 0xec28, 0x6acb, 0x86cc,
    0x186e, 0x9414, 0xd674, 0xd1a5,
];

// ---------------------------------------------------------------------------
// Small checksum (target-to-target matching, always 4-byte window)
// ---------------------------------------------------------------------------

/// Compute the small checksum for 4 bytes at `base`.
///
/// Exact match of xdelta3's `xd3_scksum`:
///   read 4 bytes as native-endian u32, multiply by HASH_MULT_32.
#[inline(always)]
pub fn small_cksum(base: &[u8]) -> u32 {
    debug_assert!(base.len() >= 4);
    let val = read_u32_ne(base);
    val.wrapping_mul(HASH_MULT_32)
}

/// Update the small checksum by shifting one byte forward.
///
/// Exact match of xdelta3's `xd3_small_cksum_update`:
///   just re-read 4 bytes at base+1 and multiply.  NOT a rolling update.
#[inline(always)]
pub fn small_cksum_update(base: &[u8]) -> u32 {
    debug_assert!(base.len() >= 5);
    let val = read_u32_ne(&base[1..]);
    val.wrapping_mul(HASH_MULT_32)
}

/// Unchecked small_cksum: caller guarantees at least 4 bytes at `ptr`.
///
/// # Safety
/// `ptr` must point to at least 4 readable bytes.
#[inline(always)]
pub unsafe fn small_cksum_at(ptr: *const u8) -> u32 {
    let val = unsafe { std::ptr::read_unaligned(ptr as *const u32) };
    val.wrapping_mul(HASH_MULT_32)
}

/// Read 4 bytes as a native-endian u32 (matches UNALIGNED_READ32).
#[inline(always)]
fn read_u32_ne(data: &[u8]) -> u32 {
    debug_assert!(data.len() >= 4);
    // Safety: All callers (small_cksum, small_cksum_update) guarantee len >= 4
    // via their own debug_assert. This eliminates the bounds check in the
    // per-byte hot loop of find_matches().
    unsafe { std::ptr::read_unaligned(data.as_ptr() as *const u32) }
}

// ---------------------------------------------------------------------------
// Large checksum (Adler-style rolling checksum, source matching)
// ---------------------------------------------------------------------------

/// Large checksum state for source matching.
///
/// Matches historical xdelta3 (`ADLER_LARGE_CKSUM=1`, `HASH_PERMUTE=1`).
#[derive(Clone)]
pub struct LargeHash {
    /// Window width (LLOOK from matcher profile).
    pub look: usize,
    look_u32: u32,
}

impl LargeHash {
    /// Build checksum state for the given window width.
    pub fn new(look: usize) -> Self {
        Self {
            look,
            look_u32: look as u32,
        }
    }

    /// Full checksum of `look` bytes starting at `base`.
    ///
    /// Equivalent to xdelta3's `xd3_lcksum` in Adler mode:
    /// low/high are accumulated with permuted input bytes, then truncated
    /// to 16 bits each into a 32-bit checksum.
    #[inline]
    pub fn checksum(&self, base: &[u8]) -> u64 {
        debug_assert!(base.len() >= self.look);
        let mut low: u32 = 0;
        let mut high: u32 = 0;
        for &b in &base[..self.look] {
            low = low.wrapping_add(SINGLE_HASH[b as usize] as u32);
            high = high.wrapping_add(low);
        }
        (((high & 0xFFFF) << 16) | (low & 0xFFFF)) as u64
    }

    /// Rolling update: remove `base[0]`, add `base[look]`.
    #[inline(always)]
    pub fn update(&self, old: u64, base: &[u8]) -> u64 {
        debug_assert!(base.len() > self.look);
        let cksum = old as u32;
        let old_c = SINGLE_HASH[base[0] as usize] as u32;
        let new_c = SINGLE_HASH[base[self.look] as usize] as u32;

        let low = cksum.wrapping_sub(old_c).wrapping_add(new_c) & 0xFFFF;
        let high = (cksum >> 16)
            .wrapping_sub(old_c.wrapping_mul(self.look_u32))
            .wrapping_add(low)
            & 0xFFFF;

        ((high << 16) | low) as u64
    }

    /// Unchecked rolling update: caller guarantees `ptr` has at least `look + 1` bytes.
    ///
    /// # Safety
    /// `ptr` must point to at least `self.look + 1` readable bytes.
    #[inline(always)]
    pub unsafe fn update_at(&self, old: u64, ptr: *const u8) -> u64 {
        let cksum = old as u32;
        let old_c = SINGLE_HASH[unsafe { *ptr } as usize] as u32;
        let new_c = SINGLE_HASH[unsafe { *ptr.add(self.look) } as usize] as u32;

        let low = cksum.wrapping_sub(old_c).wrapping_add(new_c) & 0xFFFF;
        let high = (cksum >> 16)
            .wrapping_sub(old_c.wrapping_mul(self.look_u32))
            .wrapping_add(low)
            & 0xFFFF;

        ((high << 16) | low) as u64
    }
}

// ---------------------------------------------------------------------------
// Bucket index computation
// ---------------------------------------------------------------------------

/// Hash table configuration (matches `xd3_hash_cfg`).
#[derive(Clone, Debug)]
pub struct HashCfg {
    /// Number of buckets (power of 2).
    pub size: usize,
    /// Bit shift: `32 - log2(size)` (xdelta3 historical hash domain).
    pub shift: u32,
    /// `size - 1`.
    pub mask: u64,
}

impl HashCfg {
    /// Create a hash config for the given number of slots.
    ///
    /// Matches xdelta3's `xd3_size_hashtable_bits` with compaction=1:
    /// find smallest power-of-2 >= slots, then use one bit less.
    pub fn new(slots: usize) -> Self {
        let bits = size_hashtable_bits(slots);
        let size = 1usize << bits;
        Self {
            size,
            // Match xdelta3 hash table indexing: 32-bit checksum domain.
            shift: 32 - bits as u32,
            mask: (size as u64) - 1,
        }
    }

    /// Compute bucket index from a checksum.
    ///
    /// `(cksum >> shift) ^ (cksum & mask)` — folds high bits into range.
    #[inline(always)]
    pub fn bucket(&self, cksum: u64) -> usize {
        let c = (cksum as u32) as u64;
        ((c >> self.shift) ^ (c & self.mask)) as usize
    }
}

/// Compute hash table bit width from slot count.
///
/// Matches xdelta3's `xd3_size_hashtable_bits`.
fn size_hashtable_bits(slots: usize) -> usize {
    // Match historical xdelta3 implementation: cap table lgsize at 28.
    let max_bits = 28usize;
    for i in 3..=max_bits {
        // Match xdelta3 exactly: use strict `<` so exact powers of two
        // keep their full size (instead of being halved).
        if slots < (1 << i) {
            return i - 1; // compaction = 1
        }
    }
    max_bits
}

// ---------------------------------------------------------------------------
// Run-length detection
// ---------------------------------------------------------------------------

/// Detect a run of identical bytes at the end of `seg[..look]`.
///
/// Returns `(run_length, run_byte)`.  Matches xdelta3's `xd3_comprun`.
#[inline]
pub fn comprun(seg: &[u8], look: usize) -> (usize, u8) {
    debug_assert!(seg.len() >= look);
    let mut run_l: usize = 0;
    let mut run_c: u8 = 0;
    for &byte in seg.iter().take(look) {
        if byte == run_c {
            run_l += 1;
        } else {
            run_c = byte;
            run_l = 1;
        }
    }
    (run_l, run_c)
}

// ---------------------------------------------------------------------------
// SIMD-accelerated forward match comparison
// ---------------------------------------------------------------------------

/// Compare `s1[..n]` and `s2[..n]`, return number of matching bytes from start.
///
/// Uses platform-specific SIMD when available:
/// - x86_64 AVX2: 32 bytes at a time
/// - x86_64 SSE2: 16 bytes at a time
/// - aarch64 NEON: 16 bytes at a time
/// - Fallback: 8 bytes at a time via u64 comparison
#[inline]
pub fn forward_match(s1: &[u8], s2: &[u8], n: usize) -> usize {
    forward_match_fn()(s1, s2, n.min(s1.len()).min(s2.len()))
}

/// Get the best forward-match implementation for the current CPU.
#[inline]
pub fn forward_match_fn() -> MatchFn {
    #[cfg(target_arch = "x86_64")]
    {
        return forward_match_x86_dispatch();
    }

    #[cfg(target_arch = "aarch64")]
    {
        return forward_match_neon_call;
    }

    #[allow(unreachable_code)]
    forward_match_scalar
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn forward_match_x86_dispatch() -> fn(&[u8], &[u8], usize) -> usize {
    use std::sync::OnceLock;
    static DISPATCH: OnceLock<MatchFn> = OnceLock::new();
    *DISPATCH.get_or_init(|| {
        if is_x86_feature_detected!("avx2") {
            forward_match_avx2_call
        } else if is_x86_feature_detected!("sse2") {
            forward_match_sse2_call
        } else {
            forward_match_scalar
        }
    })
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn forward_match_avx2_call(s1: &[u8], s2: &[u8], n: usize) -> usize {
    // Safety: CPU feature is checked once in dispatcher initialization.
    unsafe { forward_match_avx2(s1, s2, n) }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn forward_match_sse2_call(s1: &[u8], s2: &[u8], n: usize) -> usize {
    // Safety: CPU feature is checked once in dispatcher initialization.
    unsafe { forward_match_sse2(s1, s2, n) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn forward_match_neon_call(s1: &[u8], s2: &[u8], n: usize) -> usize {
    // Safety: NEON is mandatory on aarch64.
    unsafe { forward_match_neon(s1, s2, n) }
}

/// Scalar fallback: compare 8 bytes at a time using u64 XOR.
#[inline]
fn forward_match_scalar(s1: &[u8], s2: &[u8], n: usize) -> usize {
    let mut i = 0;
    let p1 = s1.as_ptr();
    let p2 = s2.as_ptr();

    // Compare 8 bytes at a time.
    while i + 8 <= n {
        // Safety: loop guard ensures i..i+8 in bounds for both slices.
        let a = unsafe { std::ptr::read_unaligned(p1.add(i) as *const u64) };
        // Safety: loop guard ensures i..i+8 in bounds for both slices.
        let b = unsafe { std::ptr::read_unaligned(p2.add(i) as *const u64) };
        let xor = a ^ b;
        if xor != 0 {
            // Find first differing byte.
            let diff_byte = if cfg!(target_endian = "little") {
                (xor.trailing_zeros() / 8) as usize
            } else {
                (xor.leading_zeros() / 8) as usize
            };
            return i + diff_byte;
        }
        i += 8;
    }

    // Tail: byte by byte.
    while i < n && s1[i] == s2[i] {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// x86_64 AVX2 forward match (32 bytes at a time)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn forward_match_avx2(s1: &[u8], s2: &[u8], n: usize) -> usize {
    use std::arch::x86_64::*;
    let mut i = 0;

    unsafe {
        while i + 32 <= n {
            let a = _mm256_loadu_si256(s1.as_ptr().add(i) as *const __m256i);
            let b = _mm256_loadu_si256(s2.as_ptr().add(i) as *const __m256i);
            let cmp = _mm256_cmpeq_epi8(a, b);
            let mask = _mm256_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF_FFFF {
                return i + (!mask).trailing_zeros() as usize;
            }
            i += 32;
        }

        if i + 16 <= n {
            let a = _mm_loadu_si128(s1.as_ptr().add(i) as *const __m128i);
            let b = _mm_loadu_si128(s2.as_ptr().add(i) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(a, b);
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF {
                return i + (!(mask as u16)).trailing_zeros() as usize;
            }
            i += 16;
        }
    }

    while i < n && s1[i] == s2[i] {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// x86_64 SSE2 forward match (16 bytes at a time)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn forward_match_sse2(s1: &[u8], s2: &[u8], n: usize) -> usize {
    use std::arch::x86_64::*;
    let mut i = 0;

    unsafe {
        while i + 16 <= n {
            let a = _mm_loadu_si128(s1.as_ptr().add(i) as *const __m128i);
            let b = _mm_loadu_si128(s2.as_ptr().add(i) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(a, b);
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF {
                return i + (!(mask as u16)).trailing_zeros() as usize;
            }
            i += 16;
        }
    }

    while i < n && s1[i] == s2[i] {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// aarch64 NEON forward match (16 bytes at a time)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
unsafe fn forward_match_neon(s1: &[u8], s2: &[u8], n: usize) -> usize {
    use std::arch::aarch64::*;
    let mut i = 0;

    while i + 16 <= n {
        let a = vld1q_u8(s1.as_ptr().add(i));
        let b = vld1q_u8(s2.as_ptr().add(i));
        let cmp = vceqq_u8(a, b);
        let min_val = vminvq_u8(cmp);
        if min_val == 0 {
            // At least one byte differs — find it.
            // Extract comparison result and scan.
            let not_eq = vmvnq_u8(cmp);
            // Use saturating add to find first non-zero byte.
            let mut arr = [0u8; 16];
            vst1q_u8(arr.as_mut_ptr(), not_eq);
            for j in 0..16 {
                if arr[j] != 0 {
                    return i + j;
                }
            }
        }
        i += 16;
    }

    while i < n && s1[i] == s2[i] {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// SIMD-accelerated backward match comparison
// ---------------------------------------------------------------------------

/// Compare `s1[..n]` and `s2[..n]` backwards, return number of matching bytes from end.
///
/// Uses platform-specific SIMD when available (same dispatch as forward_match).
#[inline]
pub fn backward_match(s1: &[u8], s2: &[u8], n: usize) -> usize {
    backward_match_fn()(s1, s2, n.min(s1.len()).min(s2.len()))
}

/// Get the best backward-match implementation for the current CPU.
#[inline]
pub fn backward_match_fn() -> MatchFn {
    #[cfg(target_arch = "x86_64")]
    {
        return backward_match_x86_dispatch();
    }

    #[cfg(target_arch = "aarch64")]
    {
        return backward_match_neon_call;
    }

    #[allow(unreachable_code)]
    backward_match_scalar
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn backward_match_x86_dispatch() -> fn(&[u8], &[u8], usize) -> usize {
    use std::sync::OnceLock;
    static DISPATCH: OnceLock<MatchFn> = OnceLock::new();
    *DISPATCH.get_or_init(|| {
        if is_x86_feature_detected!("avx2") {
            backward_match_avx2_call
        } else if is_x86_feature_detected!("sse2") {
            backward_match_sse2_call
        } else {
            backward_match_scalar
        }
    })
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn backward_match_avx2_call(s1: &[u8], s2: &[u8], n: usize) -> usize {
    // Safety: CPU feature is checked once in dispatcher initialization.
    unsafe { backward_match_avx2(s1, s2, n) }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn backward_match_sse2_call(s1: &[u8], s2: &[u8], n: usize) -> usize {
    // Safety: CPU feature is checked once in dispatcher initialization.
    unsafe { backward_match_sse2(s1, s2, n) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn backward_match_neon_call(s1: &[u8], s2: &[u8], n: usize) -> usize {
    // Safety: NEON is mandatory on aarch64.
    unsafe { backward_match_neon(s1, s2, n) }
}

/// Scalar fallback: compare 8 bytes at a time from the end using u64 XOR.
#[inline]
fn backward_match_scalar(s1: &[u8], s2: &[u8], n: usize) -> usize {
    let mut i = n;
    let p1 = s1.as_ptr();
    let p2 = s2.as_ptr();

    while i >= 8 {
        // Safety: loop guard ensures i-8..i is in bounds for both slices.
        let a = unsafe { std::ptr::read_unaligned(p1.add(i - 8) as *const u64) };
        // Safety: loop guard ensures i-8..i is in bounds for both slices.
        let b = unsafe { std::ptr::read_unaligned(p2.add(i - 8) as *const u64) };
        let xor = a ^ b;
        if xor != 0 {
            let tail_match = if cfg!(target_endian = "little") {
                (xor.leading_zeros() / 8) as usize
            } else {
                (xor.trailing_zeros() / 8) as usize
            };
            return n - i + tail_match;
        }
        i -= 8;
    }

    // Tail: byte by byte from end.
    while i > 0 && s1[i - 1] == s2[i - 1] {
        i -= 1;
    }
    n - i
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn backward_match_avx2(s1: &[u8], s2: &[u8], n: usize) -> usize {
    use std::arch::x86_64::*;
    let mut i = n;

    unsafe {
        while i >= 32 {
            let a = _mm256_loadu_si256(s1.as_ptr().add(i - 32) as *const __m256i);
            let b = _mm256_loadu_si256(s2.as_ptr().add(i - 32) as *const __m256i);
            let cmp = _mm256_cmpeq_epi8(a, b);
            let mask = _mm256_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF_FFFF {
                // Find first mismatch from the high end.
                return n - i + mask.leading_ones() as usize;
            }
            i -= 32;
        }

        if i >= 16 {
            let a = _mm_loadu_si128(s1.as_ptr().add(i - 16) as *const __m128i);
            let b = _mm_loadu_si128(s2.as_ptr().add(i - 16) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(a, b);
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF {
                return n - i + (mask as u16).leading_ones() as usize;
            }
            i -= 16;
        }
    }

    while i > 0 && s1[i - 1] == s2[i - 1] {
        i -= 1;
    }
    n - i
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn backward_match_sse2(s1: &[u8], s2: &[u8], n: usize) -> usize {
    use std::arch::x86_64::*;
    let mut i = n;

    unsafe {
        while i >= 16 {
            let a = _mm_loadu_si128(s1.as_ptr().add(i - 16) as *const __m128i);
            let b = _mm_loadu_si128(s2.as_ptr().add(i - 16) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(a, b);
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF {
                return n - i + (mask as u16).leading_ones() as usize;
            }
            i -= 16;
        }
    }

    while i > 0 && s1[i - 1] == s2[i - 1] {
        i -= 1;
    }
    n - i
}

#[cfg(target_arch = "aarch64")]
unsafe fn backward_match_neon(s1: &[u8], s2: &[u8], n: usize) -> usize {
    use std::arch::aarch64::*;
    let mut i = n;

    while i >= 16 {
        let a = vld1q_u8(s1.as_ptr().add(i - 16));
        let b = vld1q_u8(s2.as_ptr().add(i - 16));
        let cmp = vceqq_u8(a, b);
        let min_val = vminvq_u8(cmp);
        if min_val == 0 {
            let not_eq = vmvnq_u8(cmp);
            let mut arr = [0u8; 16];
            vst1q_u8(arr.as_mut_ptr(), not_eq);
            for j in (0..16).rev() {
                if arr[j] != 0 {
                    return n - i + 16 - j - 1;
                }
            }
        }
        i -= 16;
    }

    while i > 0 && s1[i - 1] == s2[i - 1] {
        i -= 1;
    }
    n - i
}

// ---------------------------------------------------------------------------
// SIMD-accelerated run length detection
// ---------------------------------------------------------------------------

/// Count consecutive bytes equal to `byte` starting from `data[0]`.
///
/// Returns the length of the run (0 if first byte differs).
/// Uses SIMD broadcast+compare for fast scanning.
#[inline]
pub fn find_run_length(data: &[u8], byte: u8, max: usize) -> usize {
    run_length_fn()(data, byte, max.min(data.len()))
}

/// Get the best run-length implementation for the current CPU.
#[inline]
pub fn run_length_fn() -> RunLengthFn {
    #[cfg(target_arch = "x86_64")]
    {
        return find_run_length_x86_dispatch();
    }

    #[cfg(target_arch = "aarch64")]
    {
        return find_run_length_neon_call;
    }

    #[allow(unreachable_code)]
    find_run_length_scalar
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn find_run_length_x86_dispatch() -> fn(&[u8], u8, usize) -> usize {
    use std::sync::OnceLock;
    static DISPATCH: OnceLock<RunLengthFn> = OnceLock::new();
    *DISPATCH.get_or_init(|| {
        if is_x86_feature_detected!("avx2") {
            find_run_length_avx2_call
        } else if is_x86_feature_detected!("sse2") {
            find_run_length_sse2_call
        } else {
            find_run_length_scalar
        }
    })
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn find_run_length_avx2_call(data: &[u8], byte: u8, n: usize) -> usize {
    // Safety: CPU feature is checked once in dispatcher initialization.
    unsafe { find_run_length_avx2(data, byte, n) }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn find_run_length_sse2_call(data: &[u8], byte: u8, n: usize) -> usize {
    // Safety: CPU feature is checked once in dispatcher initialization.
    unsafe { find_run_length_sse2(data, byte, n) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn find_run_length_neon_call(data: &[u8], byte: u8, n: usize) -> usize {
    // Safety: NEON is mandatory on aarch64.
    unsafe { find_run_length_neon(data, byte, n) }
}

#[inline]
fn find_run_length_scalar(data: &[u8], byte: u8, n: usize) -> usize {
    let mut i = 0;
    while i < n && data[i] == byte {
        i += 1;
    }
    i
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_run_length_avx2(data: &[u8], byte: u8, n: usize) -> usize {
    use std::arch::x86_64::*;
    let mut i = 0;

    unsafe {
        let pattern = _mm256_set1_epi8(byte as i8);

        while i + 32 <= n {
            let chunk = _mm256_loadu_si256(data.as_ptr().add(i) as *const __m256i);
            let cmp = _mm256_cmpeq_epi8(chunk, pattern);
            let mask = _mm256_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF_FFFF {
                return i + (!mask).trailing_zeros() as usize;
            }
            i += 32;
        }

        if i + 16 <= n {
            let pattern_sse = _mm_set1_epi8(byte as i8);
            let chunk = _mm_loadu_si128(data.as_ptr().add(i) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(chunk, pattern_sse);
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF {
                return i + (!(mask as u16)).trailing_zeros() as usize;
            }
            i += 16;
        }
    }

    while i < n && data[i] == byte {
        i += 1;
    }
    i
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn find_run_length_sse2(data: &[u8], byte: u8, n: usize) -> usize {
    use std::arch::x86_64::*;
    let mut i = 0;

    unsafe {
        let pattern = _mm_set1_epi8(byte as i8);

        while i + 16 <= n {
            let chunk = _mm_loadu_si128(data.as_ptr().add(i) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(chunk, pattern);
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask != 0xFFFF {
                return i + (!(mask as u16)).trailing_zeros() as usize;
            }
            i += 16;
        }
    }

    while i < n && data[i] == byte {
        i += 1;
    }
    i
}

#[cfg(target_arch = "aarch64")]
unsafe fn find_run_length_neon(data: &[u8], byte: u8, n: usize) -> usize {
    use std::arch::aarch64::*;
    let mut i = 0;

    let pattern = vdupq_n_u8(byte);

    while i + 16 <= n {
        let chunk = vld1q_u8(data.as_ptr().add(i));
        let cmp = vceqq_u8(chunk, pattern);
        let min_val = vminvq_u8(cmp);
        if min_val == 0 {
            let not_eq = vmvnq_u8(cmp);
            let mut arr = [0u8; 16];
            vst1q_u8(arr.as_mut_ptr(), not_eq);
            for j in 0..16 {
                if arr[j] != 0 {
                    return i + j;
                }
            }
        }
        i += 16;
    }

    while i < n && data[i] == byte {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Cache prefetch utility
// ---------------------------------------------------------------------------

/// Prefetch a memory location into L1 cache (read-only hint).
///
/// No-op on platforms without prefetch support.
#[inline(always)]
pub fn prefetch_read(addr: *const u8) {
    #[cfg(target_arch = "x86_64")]
    {
        // Safety: _mm_prefetch is a hint — invalid addresses are silently ignored.
        unsafe {
            std::arch::x86_64::_mm_prefetch(addr as *const i8, std::arch::x86_64::_MM_HINT_T0);
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // Safety: prefetch is a hint — invalid addresses are silently ignored.
        unsafe {
            std::arch::asm!("prfm pldl1keep, [{}]", in(reg) addr);
        }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = addr;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_cksum_basic() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let ck1 = small_cksum(&data);
        let ck2 = small_cksum_update(&data);
        // Update should use bytes 1..5, not 0..4.
        let expected = read_u32_ne(&data[1..]).wrapping_mul(HASH_MULT_32);
        assert_eq!(ck2, expected);
        assert_ne!(ck1, ck2);
    }

    #[test]
    fn small_cksum_all_zeros() {
        let data = [0u8; 8];
        assert_eq!(small_cksum(&data), 0);
        assert_eq!(small_cksum_update(&data), 0);
    }

    #[test]
    fn large_hash_full_and_rolling() {
        let lh = LargeHash::new(9);
        let data = b"Hello, World! Extra bytes here.";
        let full = lh.checksum(data);
        // Slide by one byte: remove data[0], add data[9].
        let rolled = lh.update(full, data);
        let full2 = lh.checksum(&data[1..]);
        assert_eq!(rolled, full2, "rolling update must equal fresh checksum");
    }

    #[test]
    fn large_hash_rolling_chain() {
        let lh = LargeHash::new(9);
        let data = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

        let mut h = lh.checksum(data);
        for i in 0..data.len() - lh.look {
            let expected = lh.checksum(&data[i + 1..]);
            h = lh.update(h, &data[i..]);
            assert_eq!(h, expected, "mismatch at offset {i}");
        }
    }

    #[test]
    fn hash_cfg_power_of_two() {
        let cfg = HashCfg::new(1000);
        assert!(cfg.size.is_power_of_two());
        assert!(cfg.size >= 256); // compaction: one bit less than needed
        assert_eq!(cfg.mask, cfg.size as u64 - 1);
    }

    #[test]
    fn hash_cfg_bucket_in_range() {
        let cfg = HashCfg::new(1024);
        for cksum in [0u64, 1, 127, 128, 0xDEAD_BEEF, u64::MAX] {
            let b = cfg.bucket(cksum);
            assert!(
                b < cfg.size,
                "bucket {b} out of range for size {}",
                cfg.size
            );
        }
    }

    #[test]
    fn hash_cfg_size_hashtable_bits() {
        // Matches xdelta3 behavior: bits = i-1 where slots < 2^i.
        assert_eq!(size_hashtable_bits(8), 3); // 8 < 2^4, bits=3
        assert_eq!(size_hashtable_bits(9), 3); // 9 < 2^4, bits=3
        assert_eq!(size_hashtable_bits(1000), 9); // 1000 < 2^10, bits=9
        assert_eq!(size_hashtable_bits(1024), 10); // exact power-of-two keeps full size
        assert_eq!(size_hashtable_bits(1025), 10); // 1025 < 2^11, bits=10
    }

    #[test]
    fn comprun_all_same() {
        let data = vec![0xAA; 10];
        let (len, byte) = comprun(&data, 10);
        assert_eq!(len, 10);
        assert_eq!(byte, 0xAA);
    }

    #[test]
    fn comprun_trailing_run() {
        let data = [1, 2, 3, 3, 3, 3];
        let (len, byte) = comprun(&data, 6);
        assert_eq!(len, 4);
        assert_eq!(byte, 3);
    }

    #[test]
    fn comprun_no_run() {
        let data = [1, 2, 3, 4];
        let (len, byte) = comprun(&data, 4);
        assert_eq!(len, 1);
        assert_eq!(byte, 4);
    }

    #[test]
    fn forward_match_identical() {
        let a = vec![0x55u8; 1024];
        let b = vec![0x55u8; 1024];
        assert_eq!(forward_match(&a, &b, 1024), 1024);
    }

    #[test]
    fn forward_match_differ_at_start() {
        let a = [1, 2, 3, 4];
        let b = [0, 2, 3, 4];
        assert_eq!(forward_match(&a, &b, 4), 0);
    }

    #[test]
    fn forward_match_differ_in_middle() {
        let a = vec![0xAAu8; 100];
        let mut b = vec![0xAAu8; 100];
        b[50] = 0xBB;
        assert_eq!(forward_match(&a, &b, 100), 50);
    }

    #[test]
    fn forward_match_empty() {
        assert_eq!(forward_match(&[], &[], 0), 0);
    }

    #[test]
    fn forward_match_simd_boundary() {
        // Test at exact AVX2 (32), SSE2 (16), and u64 (8) boundaries.
        for boundary in [8, 16, 32, 64, 128] {
            let a = vec![0x42u8; boundary + 5];
            let mut b = vec![0x42u8; boundary + 5];
            b[boundary] = 0xFF;
            assert_eq!(
                forward_match(&a, &b, boundary + 5),
                boundary,
                "failed at boundary {boundary}"
            );
        }
    }

    #[test]
    fn forward_match_large() {
        // 1 MB match with difference near the end.
        let n = 1 << 20;
        let a = vec![0x77u8; n];
        let mut b = vec![0x77u8; n];
        b[n - 3] = 0x99;
        assert_eq!(forward_match(&a, &b, n), n - 3);
    }

    // --- backward_match tests ---

    #[test]
    fn backward_match_identical() {
        let a = vec![0x55u8; 1024];
        let b = vec![0x55u8; 1024];
        assert_eq!(backward_match(&a, &b, 1024), 1024);
    }

    #[test]
    fn backward_match_differ_at_end() {
        let a = [1, 2, 3, 4];
        let b = [1, 2, 3, 0];
        assert_eq!(backward_match(&a, &b, 4), 0);
    }

    #[test]
    fn backward_match_differ_at_start() {
        let a = [0, 2, 3, 4];
        let b = [1, 2, 3, 4];
        assert_eq!(backward_match(&a, &b, 4), 3);
    }

    #[test]
    fn backward_match_differ_in_middle() {
        let a = vec![0xAAu8; 100];
        let mut b = vec![0xAAu8; 100];
        b[50] = 0xBB;
        assert_eq!(backward_match(&a, &b, 100), 49);
    }

    #[test]
    fn backward_match_empty() {
        assert_eq!(backward_match(&[], &[], 0), 0);
    }

    #[test]
    fn backward_match_simd_boundary() {
        for boundary in [8, 16, 32, 64, 128] {
            let a = vec![0x42u8; boundary + 5];
            let mut b = vec![0x42u8; boundary + 5];
            // Differ at position 0 — so match from end = boundary+5-1 = boundary+4.
            b[0] = 0xFF;
            assert_eq!(
                backward_match(&a, &b, boundary + 5),
                boundary + 4,
                "failed at boundary {boundary}"
            );
        }
    }

    #[test]
    fn backward_match_large() {
        let n = 1 << 20;
        let a = vec![0x77u8; n];
        let mut b = vec![0x77u8; n];
        b[2] = 0x99;
        assert_eq!(backward_match(&a, &b, n), n - 3);
    }

    // --- find_run_length tests ---

    #[test]
    fn find_run_length_all_same() {
        let data = vec![0xAA; 1024];
        assert_eq!(find_run_length(&data, 0xAA, 1024), 1024);
    }

    #[test]
    fn find_run_length_no_match() {
        let data = [1, 2, 3, 4];
        assert_eq!(find_run_length(&data, 0, 4), 0);
    }

    #[test]
    fn find_run_length_partial() {
        let mut data = vec![0xBB; 100];
        data[50] = 0xCC;
        assert_eq!(find_run_length(&data, 0xBB, 100), 50);
    }

    #[test]
    fn find_run_length_simd_boundary() {
        for boundary in [8, 16, 32, 64, 128] {
            let mut data = vec![0x42u8; boundary + 5];
            data[boundary] = 0xFF;
            assert_eq!(
                find_run_length(&data, 0x42, boundary + 5),
                boundary,
                "failed at boundary {boundary}"
            );
        }
    }

    #[test]
    fn find_run_length_empty() {
        assert_eq!(find_run_length(&[], 0, 0), 0);
    }

    #[test]
    fn find_run_length_max_limit() {
        let data = vec![0xAA; 1024];
        assert_eq!(find_run_length(&data, 0xAA, 100), 100);
    }
}
