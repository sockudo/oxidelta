// Hash tables for xdelta matching.
//
// Two tables:
//   - **Small table** (`SmallTable`): target-to-target matching.
//     Stores positions keyed by small checksum, with optional chaining
//     via a `prev` array (circular buffer indexed by `pos & mask`).
//   - **Large table** (`LargeTable`): source-to-target matching.
//     No chaining — last write wins (source positions overwrite).
//
// Both use HASH_CKOFFSET=1 so that stored value 0 means "empty".

use super::rolling::{HASH_CKOFFSET, HashCfg};

// ---------------------------------------------------------------------------
// Small hash table (target matching)
// ---------------------------------------------------------------------------

/// Small hash table with optional chaining via `prev` array.
///
/// Matches xdelta3's `small_table` + `small_prev`.
pub struct SmallTable {
    /// Bucket array: `table[bucket] = pos + HASH_CKOFFSET` or 0 (empty).
    table: Vec<u32>,
    /// Hash configuration.
    cfg: HashCfg,
    /// Previous-position chain (circular buffer).
    /// `prev[pos & mask].last_pos` = previous entry in this bucket.
    /// Only allocated when chaining is needed (small_chain > 1).
    prev: Option<Vec<u32>>,
    /// Mask for prev array indexing: `sprevsz - 1`.
    prev_mask: usize,
}

impl SmallTable {
    /// Create a new small table.
    ///
    /// `slots`: number of expected entries (determines bucket count).
    /// `sprevsz`: size of the prev chain array (must be power of 2, or 0 for no chaining).
    pub fn new(slots: usize, sprevsz: usize) -> Self {
        let cfg = HashCfg::new(slots);
        let table = vec![0u32; cfg.size];
        let (prev, prev_mask) = if sprevsz > 0 {
            debug_assert!(sprevsz.is_power_of_two());
            (Some(vec![0u32; sprevsz]), sprevsz - 1)
        } else {
            (None, 0)
        };
        Self {
            table,
            cfg,
            prev,
            prev_mask,
        }
    }

    /// Reset the table for a new window (zero all buckets and chains).
    pub fn reset(&mut self) {
        self.table.fill(0);
        if let Some(ref mut prev) = self.prev {
            prev.fill(0);
        }
    }

    /// Look up the most recent position stored for `cksum`.
    /// Returns `Some(pos)` (without HASH_CKOFFSET) or `None` if empty.
    #[inline(always)]
    pub fn lookup(&self, cksum: u64) -> Option<u64> {
        let bucket = self.cfg.bucket(cksum);
        // Safety: bucket() returns value < cfg.size, and table.len() == cfg.size.
        let val = unsafe { *self.table.get_unchecked(bucket) };
        if val != 0 {
            Some(val as u64 - HASH_CKOFFSET)
        } else {
            None
        }
    }

    /// Insert a position into the table.
    ///
    /// If chaining is enabled, the old head is saved in `prev[pos & mask]`.
    #[inline(always)]
    pub fn insert(&mut self, cksum: u64, pos: u64) {
        let stored = match u32::try_from(pos + HASH_CKOFFSET) {
            Ok(v) => v,
            Err(_) => return,
        };
        let bucket = self.cfg.bucket(cksum);
        // Safety: bucket() returns value < cfg.size, and table.len() == cfg.size.
        unsafe {
            if let Some(ref mut prev) = self.prev {
                let old_head = *self.table.get_unchecked(bucket);
                let prev_idx = pos as usize & self.prev_mask;
                *prev.get_unchecked_mut(prev_idx) = old_head;
            }
            *self.table.get_unchecked_mut(bucket) = stored;
        }
    }

    /// Walk the chain from `pos`, returning the previous entry's position.
    /// Returns `None` if the chain ends or wraps.
    ///
    /// `current_input_pos` is used to detect stale entries.
    #[inline]
    pub fn chain_prev(&self, pos: u64, current_input_pos: u64) -> Option<u64> {
        let prev = self.prev.as_ref()?;
        let prev_idx = pos as usize & self.prev_mask;
        // Safety: prev.len() == sprevsz (power of 2), mask == sprevsz - 1,
        // so prev_idx = pos & mask < sprevsz == prev.len().
        let prev_val = unsafe { *prev.get_unchecked(prev_idx) };
        if prev_val == 0 {
            return None;
        }
        let prev_pos = prev_val as u64 - HASH_CKOFFSET;
        // Reject if the chain entry is newer than current (wrapped circular buffer).
        if prev_pos > pos {
            return None;
        }
        // Reject if too far back (beyond prev array coverage).
        let diff = current_input_pos - prev_pos;
        if diff > self.prev_mask as u64 {
            return None;
        }
        Some(prev_pos)
    }

    /// Bucket count.
    pub fn size(&self) -> usize {
        self.cfg.size
    }

    /// The hash config.
    pub fn cfg(&self) -> &HashCfg {
        &self.cfg
    }

    /// Prefetch the bucket for a given checksum into L1 cache.
    #[inline(always)]
    pub fn prefetch_bucket(&self, cksum: u64) {
        let bucket = self.cfg.bucket(cksum);
        // Safety: table is always allocated with cfg.size elements,
        // and bucket() always returns a value < cfg.size.
        let addr = unsafe { self.table.as_ptr().add(bucket) as *const u8 };
        super::rolling::prefetch_read(addr);
    }
}

// ---------------------------------------------------------------------------
// Large hash table (source matching)
// ---------------------------------------------------------------------------

/// Large hash table for source checksums.
///
/// No chaining — last write wins.  Never reset between windows (source
/// checksums persist for the lifetime of the stream).
pub struct LargeTable {
    /// Bucket array: `table[bucket] = absolute_src_pos + HASH_CKOFFSET` or 0.
    table: Vec<u64>,
    /// Hash configuration.
    cfg: HashCfg,
}

impl LargeTable {
    /// Create a new large table.
    ///
    /// `slots`: expected number of source checksum entries.
    pub fn new(slots: usize) -> Self {
        let cfg = HashCfg::new(slots.max(8));
        let table = vec![0u64; cfg.size];
        Self { table, cfg }
    }

    /// Look up a source position by checksum.
    /// Returns `Some(absolute_position)` or `None` if empty.
    #[inline(always)]
    pub fn lookup(&self, cksum: u64) -> Option<u64> {
        let bucket = self.cfg.bucket(cksum);
        // Safety: bucket() returns value < cfg.size, and table.len() == cfg.size.
        let val = unsafe { *self.table.get_unchecked(bucket) };
        if val != 0 {
            Some(val - HASH_CKOFFSET)
        } else {
            None
        }
    }

    /// Insert a source position for the given checksum.
    /// Overwrites any previous entry in the same bucket.
    #[inline(always)]
    pub fn insert(&mut self, cksum: u64, pos: u64) {
        let bucket = self.cfg.bucket(cksum);
        // Safety: bucket() returns value < cfg.size, and table.len() == cfg.size.
        unsafe { *self.table.get_unchecked_mut(bucket) = pos + HASH_CKOFFSET };
    }

    /// Bucket count.
    pub fn size(&self) -> usize {
        self.cfg.size
    }

    /// The hash config.
    pub fn cfg(&self) -> &HashCfg {
        &self.cfg
    }

    /// Prefetch the bucket for a given checksum into L1 cache.
    #[inline(always)]
    pub fn prefetch_bucket(&self, cksum: u64) {
        let bucket = self.cfg.bucket(cksum);
        // Safety: table is always allocated with cfg.size elements,
        // and bucket() always returns a value < cfg.size.
        let addr = unsafe { self.table.as_ptr().add(bucket) as *const u8 };
        super::rolling::prefetch_read(addr);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_table_insert_lookup() {
        let mut t = SmallTable::new(1024, 0);
        assert!(t.lookup(42).is_none());
        t.insert(42, 100);
        assert_eq!(t.lookup(42), Some(100));
    }

    #[test]
    fn small_table_overwrite() {
        let mut t = SmallTable::new(1024, 0);
        t.insert(42, 100);
        t.insert(42, 200);
        assert_eq!(t.lookup(42), Some(200));
    }

    #[test]
    fn small_table_reset() {
        let mut t = SmallTable::new(1024, 0);
        t.insert(42, 100);
        t.reset();
        assert!(t.lookup(42).is_none());
    }

    #[test]
    fn small_table_chaining() {
        let sprevsz = 256;
        let mut t = SmallTable::new(1024, sprevsz);
        // Insert two positions with the same checksum.
        t.insert(42, 10);
        t.insert(42, 50);
        // Head should be 50.
        assert_eq!(t.lookup(42), Some(50));
        // Chain should find 10.
        let prev = t.chain_prev(50, 50);
        assert_eq!(prev, Some(10));
        // Chain from 10 should end.
        let prev2 = t.chain_prev(10, 50);
        assert!(prev2.is_none());
    }

    #[test]
    fn small_table_chain_stale_rejection() {
        let sprevsz = 16;
        let mut t = SmallTable::new(1024, sprevsz);
        t.insert(42, 0);
        t.insert(42, 100); // 100 is far beyond sprevsz=16 from pos=0
        // Chain from 100: diff = 100, mask+1 = 16 → stale.
        let prev = t.chain_prev(100, 100);
        assert!(prev.is_none());
    }

    #[test]
    fn small_table_chain_boundary_is_stale() {
        let sprevsz = 16;
        let mut t = SmallTable::new(1024, sprevsz);
        t.insert(42, 0);
        t.insert(42, 16);
        // Matches xdelta3: diff == (mask + 1) is stale.
        let prev = t.chain_prev(16, 16);
        assert!(prev.is_none());
    }

    #[test]
    fn large_table_insert_lookup() {
        let mut t = LargeTable::new(1024);
        assert!(t.lookup(99).is_none());
        t.insert(99, 5000);
        assert_eq!(t.lookup(99), Some(5000));
    }

    #[test]
    fn large_table_overwrite() {
        let mut t = LargeTable::new(1024);
        t.insert(99, 5000);
        t.insert(99, 6000);
        assert_eq!(t.lookup(99), Some(6000));
    }
}
