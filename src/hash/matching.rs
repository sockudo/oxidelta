// Block matching algorithm for delta compression.
//
// Implements the core xdelta3 string-matching loop:
//   1. Run-length detection
//   2. Large (source) match via Rabin-Karp hash
//   3. Small (target self) match via 4-byte hash + chaining
//   4. Lazy matching for improved compression
//   5. Greedy forward/backward match extension

use super::config::{MIN_MATCH, MIN_RUN, MatcherConfig};
use super::rolling::{self, LargeHash};
use super::table::{LargeTable, SmallTable};
use crate::vcdiff::code_table::Instruction;

// ---------------------------------------------------------------------------
// Match result
// ---------------------------------------------------------------------------

/// A match found by the engine, to be turned into an Instruction.
#[derive(Debug, Clone, Copy)]
pub struct Match {
    /// Position in the target where the match starts.
    pub target_pos: usize,
    /// Length of the match.
    pub length: usize,
    /// If source match: absolute offset in source.
    /// If target self-match: position in target.
    pub addr: u64,
    /// True if this is a source copy, false if target self-copy.
    pub is_source: bool,
}

// ---------------------------------------------------------------------------
// Source provider trait for the engine
// ---------------------------------------------------------------------------

/// Provides source data for match extension against source blocks.
pub trait SourceData {
    /// Total source length.
    fn len(&self) -> u64;
    /// Whether source contains no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Read bytes from the source at the given offset.
    fn get_bytes(&self, offset: u64, buf: &mut [u8]) -> usize;
    /// Direct slice access (for in-memory sources).
    fn as_slice(&self, offset: u64, len: usize) -> Option<&[u8]>;
}

impl SourceData for &[u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn get_bytes(&self, offset: u64, buf: &mut [u8]) -> usize {
        let off = offset as usize;
        if off >= <[u8]>::len(self) {
            return 0;
        }
        let avail = &self[off..];
        let n = buf.len().min(avail.len());
        buf[..n].copy_from_slice(&avail[..n]);
        n
    }
    fn as_slice(&self, offset: u64, len: usize) -> Option<&[u8]> {
        let off = offset as usize;
        if off + len <= <[u8]>::len(self) {
            Some(&self[off..off + len])
        } else {
            None
        }
    }
}

impl SourceData for Vec<u8> {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn get_bytes(&self, offset: u64, buf: &mut [u8]) -> usize {
        let s: &[u8] = self;
        s.get_bytes(offset, buf)
    }
    fn as_slice(&self, offset: u64, len: usize) -> Option<&[u8]> {
        let off = offset as usize;
        if off + len <= <[u8]>::len(self) {
            Some(&self[off..off + len])
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Match engine
// ---------------------------------------------------------------------------

/// The delta compression match engine.
///
/// Scans the input (target) data, finding matches against the source and
/// against earlier parts of the target.  Produces a sequence of
/// `Instruction` values (ADD, COPY, RUN) ready for VCDIFF encoding.
pub struct MatchEngine {
    config: MatcherConfig,
    large_hash: LargeHash,
    large_table: LargeTable,
    small_table: SmallTable,
    /// Previous-position chain size.
    _sprevsz: usize,
    /// Source position to try matching at the start of the next window.
    /// Matches xdelta3's `match_srcpos` / `MATCH_TARGET` mechanism.
    /// Initially 0, updated when a match extends to the end of a window.
    pub match_srcpos: u64,
    /// Cached CPU-dispatched match comparator.
    forward_match_fn: rolling::MatchFn,
    /// Cached CPU-dispatched backward comparator.
    backward_match_fn: rolling::MatchFn,
    /// Cached CPU-dispatched run scanner.
    run_length_fn: rolling::RunLengthFn,
}

impl MatchEngine {
    /// Create a new engine with the given matcher profile.
    ///
    /// `source_len`: total source file length (0 if no source).
    /// `winsize`: target input window size.
    pub fn new(config: MatcherConfig, source_len: u64, winsize: usize) -> Self {
        let large_hash = LargeHash::new(config.large_look);

        // Large table sizing: match xdelta3 `xd3_encode_init`.
        // It uses source max_winsize / large_step, where max_winsize is
        // rounded to power-of-two and clamped to at least XD3_ALLOCSIZE.
        const XD3_ALLOCSIZE: usize = 1 << 14; // 16 KiB
        let large_slots = if source_len > 0 {
            let src_len = source_len as usize;
            let src_max_winsize = src_len
                .checked_next_power_of_two()
                .unwrap_or(src_len)
                .max(XD3_ALLOCSIZE);
            (src_max_winsize / config.large_step).max(8)
        } else {
            8
        };
        let large_table = LargeTable::new(large_slots);

        // Small table sizing: one entry per byte of target window.
        let small_table_slots = winsize;

        // Prev chain: only needed when chain > 1.
        // Cap to the actual window size to avoid over-allocating for small inputs.
        let sprevsz = if config.small_chain > 1 || config.small_lchain > 1 {
            let max = super::config::DEFAULT_SPREVSZ;
            let capped = winsize.next_power_of_two().min(max);
            capped.max(16) // minimum 16 entries
        } else {
            0
        };

        let small_table = SmallTable::new(small_table_slots, sprevsz);

        Self {
            config,
            large_hash,
            large_table,
            small_table,
            _sprevsz: sprevsz,
            match_srcpos: 0,
            forward_match_fn: rolling::forward_match_fn(),
            backward_match_fn: rolling::backward_match_fn(),
            run_length_fn: rolling::run_length_fn(),
        }
    }

    /// Index source data into the large hash table.
    ///
    /// Checksums are inserted in reverse order within the data (matching
    /// xdelta3's `xd3_srcwin_move_point` which scans blocks backward).
    /// Last-written wins, so earlier positions take priority.
    pub fn index_source<S: SourceData>(&mut self, source: &S) {
        let src_len = source.len() as usize;
        let look = self.config.large_look;
        let step = self.config.large_step;

        if src_len < look {
            return;
        }

        // Fast path for contiguous in-memory sources (the common case).
        if let Some(src) = source.as_slice(0, src_len) {
            let mut pos = src_len - look;
            loop {
                let cksum = self.large_hash.checksum(&src[pos..]);
                self.large_table.insert(cksum, pos as u64);
                if pos < step {
                    break;
                }
                pos -= step;
            }
            return;
        }

        // Process in chunks that fit in memory (like xdelta3 block processing).
        let chunk_size = 1 << 20; // 1 MiB chunks
        let mut offset = 0usize;

        while offset < src_len {
            let end = (offset + chunk_size).min(src_len);
            let chunk_len = end - offset;

            if chunk_len < look {
                break;
            }

            if let Some(chunk) = source.as_slice(offset as u64, chunk_len) {
                // Index in reverse (last-written = earliest position wins).
                let mut pos = chunk_len - look;
                loop {
                    let cksum = self.large_hash.checksum(&chunk[pos..]);
                    self.large_table.insert(cksum, (offset + pos) as u64);

                    if pos < step {
                        break;
                    }
                    pos -= step;
                }
            }

            offset = end;
        }
    }

    /// Find all matches in `target` against `source` and earlier target data.
    ///
    /// Returns a list of instructions (ADD, COPY, RUN) covering the full target.
    pub fn find_matches<S: SourceData>(
        &mut self,
        target: &[u8],
        source: Option<&S>,
    ) -> Vec<Instruction> {
        let do_large = source.is_some();
        let do_small = true; // always do target self-matching
        let target_len = target.len();
        let use_prefetch = target_len >= (1 << 18);
        let slook = self.config.small_look;
        let llook = self.config.large_look;
        let source_len = source.map_or(0u64, |s| s.len());
        let source_contiguous = source.and_then(|s| s.as_slice(0, s.len() as usize));
        let run_length = self.run_length_fn;
        let forward_match = self.forward_match_fn;

        self.small_table.reset();

        let mut matches: Vec<Match> = Vec::with_capacity((target_len / 32).max(16));
        let mut input_pos: usize = 0;
        let mut min_match = MIN_MATCH;

        // Run-length state.
        let mut run_l: usize;
        let mut run_c: u8;

        // Checksum state.
        let mut scksum: u32;
        let mut lcksum: u64 = 0;

        if target_len < slook {
            return Self::emit_add_all(target);
        }

        // Initialize checksums at position 0.
        scksum = rolling::small_cksum(target);
        let (rl, rc) = rolling::comprun(target, slook);
        run_l = rl;
        run_c = rc;

        if do_large && target_len >= llook {
            lcksum = self.large_hash.checksum(target);
        }

        // --- Initial match probe (MATCH_TARGET) ---
        // Matches xdelta3's MATCH_TARGET mechanism: before the main loop,
        // try a forward match from match_srcpos (initially 0) against the
        // start of the target. This catches matches at source positions
        // not covered by the large hash table's step-based indexing.
        if let Some(src) = source_contiguous {
            let src_pos = self.match_srcpos as usize;
            if src_pos < src.len() {
                let max_fwd = target_len.min(src.len() - src_pos);
                if max_fwd >= MIN_MATCH {
                    let fwd_len = forward_match(&src[src_pos..], target, max_fwd);
                    if fwd_len >= MIN_MATCH {
                        matches.push(Match {
                            target_pos: 0,
                            length: fwd_len,
                            addr: src_pos as u64,
                            is_source: true,
                        });
                        input_pos = fwd_len;
                        if fwd_len == target_len {
                            self.match_srcpos = src_pos as u64 + fwd_len as u64;
                        }
                        if input_pos + slook <= target_len {
                            scksum = rolling::small_cksum(&target[input_pos..]);
                            let (rl2, rc2) = rolling::comprun(&target[input_pos..], slook);
                            run_l = rl2;
                            run_c = rc2;
                            if do_large && input_pos + llook <= target_len {
                                lcksum = self.large_hash.checksum(&target[input_pos..]);
                            }
                        }
                    }
                }
            }
        } else if let Some(src) = source {
            let src_pos = self.match_srcpos;
            if (src_pos as usize) < src.len() as usize {
                let max_fwd = target_len.min((src.len() - src_pos) as usize);
                if max_fwd >= MIN_MATCH {
                    let fwd_len = if let Some(src_slice) = src.as_slice(src_pos, max_fwd) {
                        forward_match(src_slice, target, max_fwd)
                    } else {
                        0
                    };
                    if fwd_len >= MIN_MATCH {
                        matches.push(Match {
                            target_pos: 0,
                            length: fwd_len,
                            addr: src_pos,
                            is_source: true,
                        });
                        input_pos = fwd_len;
                        // If the match extends to the end of the target,
                        // set match_srcpos for the next window.
                        if fwd_len == target_len {
                            self.match_srcpos = src_pos + fwd_len as u64;
                        }
                        // Re-initialize checksums at the new position.
                        if input_pos + slook <= target_len {
                            scksum = rolling::small_cksum(&target[input_pos..]);
                            let (rl2, rc2) = rolling::comprun(&target[input_pos..], slook);
                            run_l = rl2;
                            run_c = rc2;
                            if do_large && input_pos + llook <= target_len {
                                lcksum = self.large_hash.checksum(&target[input_pos..]);
                            }
                        }
                    }
                }
            }
        }

        loop {
            if input_pos + slook > target_len {
                break;
            }

            if use_prefetch && do_small {
                self.small_table.prefetch_bucket(scksum as u64);
            }
            if use_prefetch && do_large && input_pos + llook <= target_len {
                self.large_table.prefetch_bucket(lcksum);
            }

            // Matches xdelta3 HANDLELAZY behavior: after setting min_match
            // for lazy search, the next iteration advances by one byte without
            // decrementing min_match first.
            let mut skip_min_match_decay = false;

            // --- 1. Try RUN ---
            if run_l == slook {
                // Expand run forward (SIMD-accelerated).
                let remaining = target_len - input_pos - run_l;
                let total_run = run_l + run_length(&target[input_pos + run_l..], run_c, remaining);
                if total_run >= min_match && total_run >= MIN_RUN {
                    matches.push(Match {
                        target_pos: input_pos,
                        length: total_run,
                        addr: 0,
                        is_source: false,
                    });
                    // Mark as RUN (addr=u64::MAX sentinel).
                    matches.last_mut().unwrap().addr = u64::MAX;

                    if !try_lazy(total_run, self.config.max_lazy, input_pos, target_len) {
                        input_pos += total_run;
                        min_match = MIN_MATCH;
                        if input_pos + slook <= target_len {
                            scksum = rolling::small_cksum(&target[input_pos..]);
                            let (rl2, rc2) = rolling::comprun(&target[input_pos..], slook);
                            run_l = rl2;
                            run_c = rc2;
                            if do_large && input_pos + llook <= target_len {
                                lcksum = self.large_hash.checksum(&target[input_pos..]);
                            }
                        }
                        continue;
                    }
                    min_match = total_run;
                    skip_min_match_decay = true;
                    // Fall through to advance by 1 (lazy).
                }
            }

            // --- 2. Try LARGE (source) match ---
            if do_large
                && input_pos + llook <= target_len
                && let Some(src_pos) = self.large_table.lookup(lcksum)
            {
                let m = if let Some(src) = source_contiguous {
                    self.extend_source_match_slice(target, src, input_pos, src_pos)
                } else if let Some(src) = source {
                    self.extend_source_match(target, src, input_pos, src_pos, source_len)
                } else {
                    None
                };

                if let Some(m) = m {
                    // Match xdelta3: source matches are accepted based on
                    // forward extension length (match_fwd), not total (back+fwd).
                    let back_len = input_pos - m.target_pos;
                    let fwd_len = m.length - back_len;
                    if fwd_len >= min_match {
                        // Erase any previous matches that this backward-extended
                        // match now covers (iopt-style erasure).
                        if back_len > 0 {
                            while let Some(last) = matches.last() {
                                if last.target_pos >= m.target_pos {
                                    matches.pop();
                                } else {
                                    break;
                                }
                            }
                        }

                        matches.push(m);
                        if !try_lazy(fwd_len, self.config.max_lazy, input_pos, target_len) {
                            // Advance past the forward part only.
                            // Match covers [input_pos - back_len, input_pos + fwd_len).
                            input_pos += fwd_len;
                            min_match = MIN_MATCH;
                            if input_pos + slook <= target_len {
                                scksum = rolling::small_cksum(&target[input_pos..]);
                                let (rl2, rc2) = rolling::comprun(&target[input_pos..], slook);
                                run_l = rl2;
                                run_c = rc2;
                                if do_large && input_pos + llook <= target_len {
                                    lcksum = self.large_hash.checksum(&target[input_pos..]);
                                }
                            }
                            continue;
                        }
                        min_match = fwd_len;
                        skip_min_match_decay = true;
                    }
                }
            }

            // --- 3. Try SMALL (target self) match ---
            if do_small {
                let match_result = self.small_match(target, input_pos, scksum, min_match);

                // Always insert current position.
                self.small_table.insert(scksum as u64, input_pos as u64);

                if let Some(m) = match_result
                    && m.length >= min_match
                {
                    matches.push(m);
                    if !try_lazy(m.length, self.config.max_lazy, input_pos, target_len) {
                        input_pos += m.length;
                        min_match = MIN_MATCH;
                        if input_pos + slook <= target_len {
                            scksum = rolling::small_cksum(&target[input_pos..]);
                            let (rl2, rc2) = rolling::comprun(&target[input_pos..], slook);
                            run_l = rl2;
                            run_c = rc2;
                            if do_large && input_pos + llook <= target_len {
                                lcksum = self.large_hash.checksum(&target[input_pos..]);
                            }
                        }
                        continue;
                    }
                    min_match = m.length;
                    skip_min_match_decay = true;
                }
            } else {
                self.small_table.insert(scksum as u64, input_pos as u64);
            }

            // --- 4. Advance by 1 (lazy matching or no match found) ---
            if !skip_min_match_decay && min_match > MIN_MATCH {
                min_match -= 1;
            }

            input_pos += 1;
            if input_pos + slook > target_len {
                break;
            }

            // Incremental updates — use unchecked access since we verified bounds above.
            // Safety: input_pos >= 1, and input_pos + slook <= target.len(),
            // so target[input_pos..input_pos+4] is valid (slook=4).
            // small_cksum_at needs ptr to input_pos (4 readable bytes).
            unsafe {
                let base_ptr = target.as_ptr().add(input_pos);
                scksum = rolling::small_cksum_at(base_ptr);
            }

            // Run update — input_pos + slook - 1 < target_len is guaranteed
            // since input_pos + slook <= target_len.
            {
                let next_byte = target[input_pos + slook - 1];
                if next_byte == run_c {
                    run_l += 1;
                } else {
                    run_c = next_byte;
                    run_l = 1;
                }
            }

            if do_large && input_pos + llook <= target_len {
                // Safety: input_pos >= 1, input_pos + llook <= target_len,
                // so target[input_pos-1..input_pos-1+llook+1] is valid.
                unsafe {
                    let base_ptr = target.as_ptr().add(input_pos - 1);
                    lcksum = self.large_hash.update_at(lcksum, base_ptr);
                }
            }
        }

        // Convert matches to instructions.
        Self::matches_to_instructions(target, source_len, &matches)
    }

    // -----------------------------------------------------------------------
    // Small (target) match scanning — matches xd3_smatch
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn small_match(
        &self,
        target: &[u8],
        input_pos: usize,
        _scksum: u32,
        min_match: usize,
    ) -> Option<Match> {
        let scksum = _scksum as u64;
        let head = self.small_table.lookup(scksum)?;
        let head = head as usize;

        let is_lazy = min_match > MIN_MATCH;
        let max_chain = if is_lazy {
            self.config.small_lchain
        } else {
            self.config.small_chain
        };

        let mut best_len = 0usize;
        let mut best_offset = 0usize;
        let mut base = head;
        let mut chain = max_chain;

        loop {
            // Compare target[base..] with target[input_pos..].
            let max_cmp = target.len() - input_pos;
            let ref_start = base;
            let inp_start = input_pos;

            if ref_start >= inp_start {
                break; // can't copy from future
            }

            // VCDIFF target COPY allows overlap, so small matches are allowed
            // to extend all the way to end-of-input (matches xdelta3).
            let cmp_len_limit = max_cmp;

            let cmp_len =
                (self.forward_match_fn)(&target[ref_start..], &target[inp_start..], cmp_len_limit);

            if cmp_len > best_len {
                best_len = cmp_len;
                best_offset = base;

                if cmp_len >= self.config.long_enough || inp_start + cmp_len >= target.len() {
                    break;
                }
            }

            chain -= 1;
            if chain == 0 {
                break;
            }

            // Walk chain.
            match self.small_table.chain_prev(base as u64, input_pos as u64) {
                Some(prev) => base = prev as usize,
                None => break,
            }
        }

        if best_len < MIN_MATCH {
            return None;
        }

        // Efficiency filter: reject short matches with expensive addresses.
        // Matches xdelta3's filter in xd3_smatch.
        let distance = input_pos - best_offset;
        if best_len == 4 && distance >= 1 << 14 {
            return None;
        }
        if best_len == 5 && distance >= 1 << 21 {
            return None;
        }

        Some(Match {
            target_pos: input_pos,
            length: best_len,
            addr: best_offset as u64,
            is_source: false,
        })
    }

    // -----------------------------------------------------------------------
    // Source match extension — matches xd3_source_extend_match
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn extend_source_match_slice(
        &self,
        target: &[u8],
        source: &[u8],
        input_pos: usize,
        src_pos: u64,
    ) -> Option<Match> {
        let src_pos = src_pos as usize;
        if src_pos >= source.len() {
            return None;
        }

        let max_fwd = (target.len() - input_pos).min(source.len() - src_pos);
        let fwd_len = (self.forward_match_fn)(&source[src_pos..], &target[input_pos..], max_fwd);
        if fwd_len < MIN_MATCH {
            return None;
        }

        let max_back = input_pos.min(src_pos);
        let back_len = if max_back > 0 {
            (self.backward_match_fn)(
                &source[src_pos - max_back..src_pos],
                &target[input_pos - max_back..input_pos],
                max_back,
            )
        } else {
            0
        };

        Some(Match {
            target_pos: input_pos - back_len,
            length: back_len + fwd_len,
            addr: (src_pos - back_len) as u64,
            is_source: true,
        })
    }

    fn extend_source_match<S: SourceData>(
        &self,
        target: &[u8],
        source: &S,
        input_pos: usize,
        src_pos: u64,
        source_len: u64,
    ) -> Option<Match> {
        // Forward extension.
        let max_fwd = target.len() - input_pos;
        let src_avail = (source_len - src_pos) as usize;
        let max_fwd = max_fwd.min(src_avail);

        let fwd_len = if let Some(src_slice) = source.as_slice(src_pos, max_fwd) {
            (self.forward_match_fn)(src_slice, &target[input_pos..], max_fwd)
        } else {
            // Fallback for non-contiguous sources.
            let mut buf = [0u8; 16 * 1024];
            let mut total = 0;
            let mut off = 0usize;
            while off < max_fwd {
                let chunk = (max_fwd - off).min(buf.len());
                let n = source.get_bytes(src_pos + off as u64, &mut buf[..chunk]);
                if n == 0 {
                    break;
                }
                let m = (self.forward_match_fn)(&buf[..n], &target[input_pos + off..], n);
                total += m;
                if m < n {
                    break;
                }
                off += n;
            }
            total
        };

        if fwd_len < MIN_MATCH {
            return None;
        }

        // Backward extension (SIMD-accelerated when source is contiguous).
        let max_back = input_pos.min(src_pos as usize);
        let mut back_len = 0usize;
        if max_back > 0
            && let Some(src_slice) = source.as_slice(src_pos - max_back as u64, max_back)
        {
            let tgt_slice = &target[input_pos - max_back..input_pos];
            back_len = (self.backward_match_fn)(src_slice, tgt_slice, max_back);
        }

        let total_len = back_len + fwd_len;
        let match_start_target = input_pos - back_len;
        let match_start_source = src_pos - back_len as u64;

        Some(Match {
            target_pos: match_start_target,
            length: total_len,
            addr: match_start_source,
            is_source: true,
        })
    }

    // -----------------------------------------------------------------------
    // Convert matches to instructions
    // -----------------------------------------------------------------------

    fn emit_add_all(target: &[u8]) -> Vec<Instruction> {
        if target.is_empty() {
            return Vec::new();
        }
        vec![Instruction::Add {
            len: target.len() as u32,
        }]
    }

    fn matches_to_instructions(
        target: &[u8],
        source_len: u64,
        matches: &[Match],
    ) -> Vec<Instruction> {
        let mut instructions = Vec::with_capacity(matches.len().saturating_mul(2) + 1);
        let mut covered_to: usize = 0;

        // Sort matches by target position, preferring longer matches.
        // For overlapping matches from lazy matching, keep the best.
        let mut sorted: Vec<Match> = Vec::with_capacity(matches.len());
        for &m in matches {
            // Remove matches covered by later, better overlapping ones.
            while let Some(last) = sorted.last() {
                if last.target_pos + last.length > m.target_pos && m.length > last.length {
                    sorted.pop();
                } else {
                    break;
                }
            }
            // Only add if not fully covered by previous.
            if sorted.last().is_none_or(|last| {
                m.target_pos >= last.target_pos + last.length || m.length > last.length
            }) {
                // Trim overlap with previous.
                sorted.push(m);
            }
        }

        for m in &sorted {
            let m_start = m.target_pos;
            let m_end = m_start + m.length;

            if m_start < covered_to {
                continue; // skip fully overlapped
            }

            // Emit ADD for gap before this match.
            if m_start > covered_to {
                instructions.push(Instruction::Add {
                    len: (m_start - covered_to) as u32,
                });
            }

            // Emit the match.
            if m.addr == u64::MAX {
                // RUN instruction.
                instructions.push(Instruction::Run {
                    len: m.length as u32,
                });
            } else if m.is_source {
                // Source COPY — address is absolute source offset.
                instructions.push(Instruction::Copy {
                    len: m.length as u32,
                    addr: m.addr,
                    mode: 0,
                });
            } else {
                // Target self-copy — address in combined space = source_len + target_offset.
                instructions.push(Instruction::Copy {
                    len: m.length as u32,
                    addr: source_len + m.addr,
                    mode: 0,
                });
            }

            covered_to = m_end;
        }

        // Trailing ADD.
        if covered_to < target.len() {
            instructions.push(Instruction::Add {
                len: (target.len() - covered_to) as u32,
            });
        }

        instructions
    }
}

/// Should we try lazy matching?
///
/// Matches xdelta3's `TRYLAZYLEN(LEN, POS, MAX)`:
///   `max_lazy > 0 && len < max_lazy && pos + len <= avail_in - 2`
/// The `-2` ensures enough data remains for a lazy match to be worthwhile
/// (the next match starts at pos+1 and must match at least 2 extra bytes).
#[inline(always)]
fn try_lazy(match_len: usize, max_lazy: usize, pos: usize, avail_in: usize) -> bool {
    max_lazy > 0 && match_len < max_lazy && pos + match_len + 2 <= avail_in
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::config;

    #[test]
    fn no_source_add_only() {
        let mut engine = MatchEngine::new(config::FASTEST, 0, 1 << 16);
        let target = b"Hello, world!";
        let instructions = engine.find_matches(target, None::<&&[u8]>);
        assert!(!instructions.is_empty());
        // Should be a single ADD.
        match instructions[0] {
            Instruction::Add { len } => assert_eq!(len, target.len() as u32),
            _ => panic!("expected ADD"),
        }
    }

    #[test]
    fn source_copy_identical() {
        let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let target = source;
        let src: &[u8] = source;

        let mut engine = MatchEngine::new(config::DEFAULT, src.len() as u64, target.len());
        engine.index_source(&src);
        let instructions = engine.find_matches(target, Some(&src));

        // Should have at least one COPY instruction.
        let has_copy = instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }));
        assert!(
            has_copy,
            "expected COPY for identical data: {instructions:?}"
        );
    }

    #[test]
    fn target_self_copy() {
        let mut engine = MatchEngine::new(config::DEFAULT, 0, 1 << 16);
        // Repeating pattern — should find self-copies.
        let target = b"ABCDABCDABCDABCDABCDABCDABCDABCD";
        let instructions = engine.find_matches(target, None::<&&[u8]>);

        let has_copy = instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }));
        assert!(
            has_copy,
            "expected self-COPY for repeating data: {instructions:?}"
        );
    }

    #[test]
    fn run_detection() {
        let mut engine = MatchEngine::new(config::DEFAULT, 0, 1 << 16);
        let target = vec![0xAA; 100];
        let instructions = engine.find_matches(&target, None::<&&[u8]>);

        let has_run = instructions
            .iter()
            .any(|i| matches!(i, Instruction::Run { .. }));
        assert!(has_run, "expected RUN for constant data: {instructions:?}");
    }

    #[test]
    fn instructions_cover_full_target() {
        let source = b"The quick brown fox jumps over the lazy dog.";
        let target = b"The quick brown cat sits on the lazy mat.";
        let src: &[u8] = source;

        let mut engine = MatchEngine::new(config::DEFAULT, src.len() as u64, target.len());
        engine.index_source(&src);
        let instructions = engine.find_matches(target, Some(&src));

        // Sum of all instruction lengths must equal target length.
        let total: u32 = instructions
            .iter()
            .map(|i| match i {
                Instruction::Add { len } => *len,
                Instruction::Copy { len, .. } => *len,
                Instruction::Run { len } => *len,
            })
            .sum();
        assert_eq!(
            total,
            target.len() as u32,
            "instructions don't cover full target"
        );
    }

    #[test]
    fn small_target_no_panic() {
        let mut engine = MatchEngine::new(config::FASTEST, 0, 1 << 16);
        // Target smaller than slook (4 bytes).
        for len in 0..4 {
            let target = vec![0x42; len];
            let insts = engine.find_matches(&target, None::<&&[u8]>);
            let total: u32 = insts
                .iter()
                .map(|i| match i {
                    Instruction::Add { len } => *len,
                    Instruction::Copy { len, .. } => *len,
                    Instruction::Run { len } => *len,
                })
                .sum();
            assert_eq!(total, len as u32);
        }
    }

    #[test]
    fn empty_target() {
        let mut engine = MatchEngine::new(config::DEFAULT, 0, 1 << 16);
        let insts = engine.find_matches(b"", None::<&&[u8]>);
        assert!(insts.is_empty());
    }

    #[test]
    fn all_profiles_produce_valid_output() {
        let source = b"AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH";
        let target = b"AAAA CCCC DDDD EEEE xxxx GGGG HHHH IIII";

        for profile in [
            config::FASTEST,
            config::FASTER,
            config::FAST,
            config::DEFAULT,
            config::SLOW,
        ] {
            let src: &[u8] = source;
            let mut engine = MatchEngine::new(profile, src.len() as u64, target.len());
            engine.index_source(&src);
            let instructions = engine.find_matches(target, Some(&src));

            let total: u32 = instructions
                .iter()
                .map(|i| match i {
                    Instruction::Add { len } => *len,
                    Instruction::Copy { len, .. } => *len,
                    Instruction::Run { len } => *len,
                })
                .sum();
            assert_eq!(
                total,
                target.len() as u32,
                "profile {} produced wrong total length",
                profile.name
            );
        }
    }
}
