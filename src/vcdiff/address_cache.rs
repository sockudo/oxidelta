// VCDIFF address cache (RFC 3284, Section 5.3).
//
// Implements the NEAR and SAME address caches used to compactly encode
// COPY instruction addresses.  Byte-for-byte compatible with xdelta3's
// `xd3_addr_cache` / `xd3_encode_address` / `xd3_decode_address`.

use super::varint;

// ---------------------------------------------------------------------------
// Address modes (RFC 3284 Section 5.3)
// ---------------------------------------------------------------------------

/// Absolute address.
pub const VCD_SELF: u8 = 0;
/// Address relative to "here" (current position in address space).
pub const VCD_HERE: u8 = 1;

// ---------------------------------------------------------------------------
// Address cache
// ---------------------------------------------------------------------------

/// NEAR/SAME address cache.
///
/// Default configuration (s_near=4, s_same=3) gives 9 address modes:
///   0      VCD_SELF  — absolute
///   1      VCD_HERE  — here - value
///   2..5   NEAR      — near\[mode-2\] + value
///   6..8   SAME      — same\[(mode-6)*256 + byte\]
#[derive(Clone)]
pub struct AddressCache {
    s_near: usize,
    s_same: usize,
    near: Vec<u64>,
    same: Vec<u64>,
    next_slot: usize,
}

impl AddressCache {
    /// Default RFC 3284 cache: s_near=4, s_same=3.
    pub fn new() -> Self {
        Self::with_sizes(4, 3)
    }

    /// Create with custom cache sizes.
    pub fn with_sizes(s_near: usize, s_same: usize) -> Self {
        Self {
            s_near,
            s_same,
            near: vec![0; s_near],
            same: vec![0; s_same * 256],
            next_slot: 0,
        }
    }

    /// Reset cache state to initial (all zeros).
    /// Called at the start of each window.
    pub fn init(&mut self) {
        self.near.fill(0);
        self.same.fill(0);
        self.next_slot = 0;
    }

    /// Total number of address modes (2 + s_near + s_same).
    #[inline]
    pub fn mode_count(&self) -> usize {
        2 + self.s_near + self.s_same
    }

    /// Number of NEAR cache slots.
    #[inline]
    pub fn s_near(&self) -> usize {
        self.s_near
    }

    /// Number of SAME cache groups.
    #[inline]
    pub fn s_same(&self) -> usize {
        self.s_same
    }

    /// The first SAME mode index (2 + s_near).
    #[inline]
    fn same_start(&self) -> usize {
        2 + self.s_near
    }

    // -----------------------------------------------------------------------
    // Cache update (shared by encoder and decoder)
    //
    // Exact match of xdelta3 `xd3_update_cache`.
    // -----------------------------------------------------------------------

    /// Update the cache after encoding or decoding an address.
    #[inline]
    pub fn update(&mut self, addr: u64) {
        if self.s_near > 0 {
            self.near[self.next_slot] = addr;
            self.next_slot = (self.next_slot + 1) % self.s_near;
        }
        if self.s_same > 0 {
            let idx = addr as usize % (self.s_same * 256);
            self.same[idx] = addr;
        }
    }

    // -----------------------------------------------------------------------
    // Encoding (matches xdelta3 `xd3_encode_address`)
    // -----------------------------------------------------------------------

    /// Encode an address, selecting the best mode.
    ///
    /// Returns `(mode, encoded_bytes)`.
    ///
    /// For SELF/HERE/NEAR modes the encoded bytes are a varint.
    /// For SAME modes the encoded bytes are a single raw byte.
    ///
    /// `here` is the current cumulative decoded position in the address
    /// space (source window length + target bytes decoded so far).
    pub fn encode(&mut self, addr: u64, here: u64) -> (u8, EncodedAddr) {
        debug_assert!(addr < here);

        let mut best_d = addr;
        let mut best_m: u8 = VCD_SELF;

        // Short-circuit: if already fits in a single varint byte.
        macro_rules! smallest_int {
            ($d:expr) => {
                if $d <= 127 {
                    best_d = $d;
                    // best_m already set by caller
                    // jump to emit
                    let r = self.emit_non_same(best_d, best_m);
                    self.update(addr);
                    return r;
                }
            };
        }

        smallest_int!(best_d);

        // VCD_HERE
        let d = here - addr;
        if d < best_d {
            best_d = d;
            best_m = VCD_HERE;
            smallest_int!(best_d);
        }

        // NEAR modes
        for i in 0..self.s_near {
            if addr >= self.near[i] {
                let d = addr - self.near[i];
                if d < best_d {
                    best_d = d;
                    best_m = (i as u8) + 2;
                    smallest_int!(best_d);
                }
            }
        }

        // SAME mode
        if self.s_same > 0 {
            let d_idx = addr as usize % (self.s_same * 256);
            if self.same[d_idx] == addr {
                let byte_val = (d_idx % 256) as u8;
                let mode = (self.same_start() + d_idx / 256) as u8;
                self.update(addr);
                return (mode, EncodedAddr::SameByte(byte_val));
            }
        }

        // Fall through: emit varint for best mode found.
        let r = self.emit_non_same(best_d, best_m);
        self.update(addr);
        r
    }

    fn emit_non_same(&self, val: u64, mode: u8) -> (u8, EncodedAddr) {
        let mut buf = [0u8; 10];
        let len = varint::encode_u64(val, &mut buf);
        let mut out = [0u8; 10];
        out[..len].copy_from_slice(&buf[10 - len..]);
        (mode, EncodedAddr::VarInt { bytes: out, len })
    }

    // -----------------------------------------------------------------------
    // Decoding (matches xdelta3 `xd3_decode_address`)
    // -----------------------------------------------------------------------

    /// Decode an address given the mode and the address section data.
    ///
    /// `mode` is the address mode from the instruction (0..mode_count).
    /// `addr_data` is the remaining address section bytes.
    /// `here` is the current position in the address space.
    ///
    /// Returns `(address, bytes_consumed)` or an error.
    pub fn decode(
        &mut self,
        mode: u8,
        addr_data: &[u8],
        here: u64,
    ) -> Result<(u64, usize), AddressCacheError> {
        let mode = mode as usize;
        let same_start = self.same_start();

        let (addr, consumed) = if mode < same_start {
            // SELF, HERE, or NEAR: read a varint.
            let (raw, consumed) =
                varint::read_u64(addr_data).map_err(|_| AddressCacheError::AddrUnderflow)?;

            let addr = match mode {
                0 => raw, // VCD_SELF
                1 => {
                    here.checked_sub(raw)
                        .ok_or(AddressCacheError::InvalidAddr)? // VCD_HERE
                }
                _ => {
                    // NEAR mode
                    self.near[mode - 2]
                        .checked_add(raw)
                        .ok_or(AddressCacheError::InvalidAddr)?
                }
            };
            (addr, consumed)
        } else {
            // SAME mode: read a single raw byte.
            if addr_data.is_empty() {
                return Err(AddressCacheError::AddrUnderflow);
            }
            let slot = mode - same_start;
            let byte = addr_data[0] as usize;
            let addr = self.same[slot * 256 + byte];
            (addr, 1)
        };

        // Validate: address must be < here.
        if addr >= here {
            return Err(AddressCacheError::InvalidAddr);
        }

        self.update(addr);
        Ok((addr, consumed))
    }
}

impl Default for AddressCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Encoded address representation
// ---------------------------------------------------------------------------

/// The encoded form of an address produced by `AddressCache::encode`.
#[derive(Debug, Clone)]
pub enum EncodedAddr {
    /// A variable-length integer (SELF, HERE, NEAR modes).
    VarInt { bytes: [u8; 10], len: usize },
    /// A single raw byte (SAME mode).
    SameByte(u8),
}

impl EncodedAddr {
    /// Write the encoded bytes to a writer.
    pub fn write_to<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        match self {
            EncodedAddr::VarInt { bytes, len } => w.write_all(&bytes[..*len]),
            EncodedAddr::SameByte(b) => w.write_all(&[*b]),
        }
    }

    /// The encoded byte length.
    pub fn len(&self) -> usize {
        match self {
            EncodedAddr::VarInt { len, .. } => *len,
            EncodedAddr::SameByte(_) => 1,
        }
    }

    /// Whether the encoded representation is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Encoded bytes as a slice.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            EncodedAddr::VarInt { bytes, len } => &bytes[..*len],
            EncodedAddr::SameByte(b) => std::slice::from_ref(b),
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressCacheError {
    /// Not enough bytes in the address section.
    AddrUnderflow,
    /// Decoded address is invalid (out of range or overflow).
    InvalidAddr,
}

impl std::fmt::Display for AddressCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddrUnderflow => write!(f, "address section underflow"),
            Self::InvalidAddr => write!(f, "invalid COPY address"),
        }
    }
}

impl std::error::Error for AddressCacheError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cache_params() {
        let c = AddressCache::new();
        assert_eq!(c.s_near(), 4);
        assert_eq!(c.s_same(), 3);
        assert_eq!(c.mode_count(), 9);
    }

    #[test]
    fn encode_decode_self_mode() {
        let mut enc = AddressCache::new();
        let mut dec = AddressCache::new();

        let addr = 42u64;
        let here = 1000u64;
        let (mode, encoded) = enc.encode(addr, here);
        // For small absolute address, VCD_SELF is likely chosen.
        assert_eq!(mode, VCD_SELF);

        let (decoded, consumed) = dec.decode(mode, encoded.as_bytes(), here).unwrap();
        assert_eq!(decoded, addr);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn encode_decode_here_mode() {
        let mut enc = AddressCache::new();
        let mut dec = AddressCache::new();

        // Address close to `here` -- HERE mode should be cheaper.
        let here = 1000u64;
        let addr = 990u64; // distance = 10, which is < addr (990)
        let (mode, encoded) = enc.encode(addr, here);
        assert_eq!(mode, VCD_HERE);

        let (decoded, _) = dec.decode(mode, encoded.as_bytes(), here).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn encode_decode_near_mode() {
        let mut enc = AddressCache::new();
        let mut dec = AddressCache::new();

        // Prime the NEAR cache with a large address.
        let base = 500_000u64;
        enc.update(base);
        dec.update(base);

        // Now encode an address near `base`.
        let addr = base + 5;
        let here = 1_000_000u64;
        let (mode, encoded) = enc.encode(addr, here);
        // Should pick NEAR mode 2 (slot 1 since slot 0 advanced).
        assert!((2..6).contains(&mode), "expected NEAR mode, got {mode}");

        let (decoded, _) = dec.decode(mode, encoded.as_bytes(), here).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn encode_decode_same_mode() {
        let mut enc = AddressCache::new();
        let mut dec = AddressCache::new();

        // Put an address in the SAME cache, then flush the NEAR cache so
        // that the NEAR slots no longer contain this address.  This forces
        // the encoder to pick SAME mode.
        let addr = 12345u64;
        enc.update(addr);
        dec.update(addr);

        // Fill NEAR cache (4 slots) with other addresses to evict `addr`.
        for i in 1..=4u64 {
            enc.update(i * 1_000_000);
            dec.update(i * 1_000_000);
        }

        // Encoding the same address again should use SAME mode.
        let here = 10_000_000u64;
        let (mode, encoded) = enc.encode(addr, here);
        let same_start = enc.same_start() as u8;
        assert!(mode >= same_start, "expected SAME mode, got {mode}");
        assert_eq!(encoded.len(), 1); // single byte

        let (decoded, consumed) = dec.decode(mode, encoded.as_bytes(), here).unwrap();
        assert_eq!(decoded, addr);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn cache_init_resets() {
        let mut c = AddressCache::new();
        c.update(999);
        c.init();
        // After init, near and same should be zeroed.
        assert!(c.near.iter().all(|&x| x == 0));
        assert!(c.same.iter().all(|&x| x == 0));
        assert_eq!(c.next_slot, 0);
    }

    #[test]
    fn near_cache_is_circular() {
        let mut c = AddressCache::new();
        // Fill 5 entries into a 4-slot NEAR cache.
        for i in 0..5u64 {
            c.update(i * 100);
        }
        // Slot 0 should have been overwritten by the 5th update.
        assert_eq!(c.near[0], 400);
        assert_eq!(c.near[1], 100);
        assert_eq!(c.near[2], 200);
        assert_eq!(c.near[3], 300);
    }

    #[test]
    fn roundtrip_many_addresses() {
        let mut enc = AddressCache::new();
        let mut dec = AddressCache::new();

        let addresses = [0u64, 4, 100, 4, 100, 50000, 50004, 50000, 1, 99999];
        let mut here = 100_000u64;

        for &addr in &addresses {
            let (mode, encoded) = enc.encode(addr, here);
            let (decoded, _) = dec.decode(mode, encoded.as_bytes(), here).unwrap();
            assert_eq!(decoded, addr, "mismatch at here={here}, addr={addr}");
            here += 100; // advance position
        }
    }
}
