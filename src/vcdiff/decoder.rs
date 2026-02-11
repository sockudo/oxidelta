// VCDIFF decoder: instruction decoding and window reconstruction.
//
// Byte-for-byte compatible with xdelta3's decoder.  Follows the same
// state progression: parse file header, then for each window parse
// the window header, read sections, execute instructions.
//
// Performance notes:
//   - Section buffers (data/inst/addr) are reused across windows in StreamDecoder
//   - Source COPY uses zero-copy slice access when source is in memory
//   - A reusable copy_buf handles non-contiguous sources without per-COPY allocation
//   - Output Vec is pre-sized to target_window_len

use std::io::Read;

use super::address_cache::AddressCache;
use super::code_table::{self, CodeTable, Instruction, XD3_ADD, XD3_CPY, XD3_NOOP, XD3_RUN};
use super::header::{FileHeader, VCD_TARGET, WindowHeader};
use super::varint;

// ---------------------------------------------------------------------------
// Decoder error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    InvalidInput(String),
    ChecksumMismatch { expected: u32, actual: u32 },
    Unsupported(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: expected {expected:#010X}, got {actual:#010X}"
                )
            }
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Source provider trait
// ---------------------------------------------------------------------------

/// Provides source data for COPY instructions that reference source bytes.
pub trait SourceProvider {
    /// Read bytes from the source at absolute offset `offset` into `buf`.
    /// Returns the number of bytes actually read.
    fn read_source(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DecodeError>;

    /// Total source length (if known).
    fn source_len(&self) -> Option<u64>;

    /// Direct zero-copy slice access for in-memory sources.
    ///
    /// Returns `Some(slice)` if the source data at `[offset..offset+len]`
    /// is available as a contiguous memory slice. Returns `None` if the
    /// source is not contiguous (e.g., file-backed, chunked).
    ///
    /// When available, the decoder uses this to avoid intermediate buffer
    /// allocations on every COPY instruction.
    fn source_slice(&self, _offset: u64, _len: usize) -> Option<&[u8]> {
        None
    }
}

/// In-memory source.
impl SourceProvider for &[u8] {
    fn read_source(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DecodeError> {
        let offset = offset as usize;
        if offset >= self.len() {
            return Ok(0);
        }
        let available = &self[offset..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(n)
    }

    fn source_len(&self) -> Option<u64> {
        Some(self.len() as u64)
    }

    fn source_slice(&self, offset: u64, len: usize) -> Option<&[u8]> {
        let offset = offset as usize;
        if offset + len <= self.len() {
            Some(&self[offset..offset + len])
        } else {
            None
        }
    }
}

/// No-source provider (for delta streams without a source file).
pub struct NoSource;

impl SourceProvider for NoSource {
    fn read_source(&mut self, _offset: u64, _buf: &mut [u8]) -> Result<usize, DecodeError> {
        Err(DecodeError::InvalidInput(
            "COPY references source but no source provided".into(),
        ))
    }

    fn source_len(&self) -> Option<u64> {
        None
    }
}

// ---------------------------------------------------------------------------
// Window decoder
// ---------------------------------------------------------------------------

/// Borrowed DATA/INST/ADDR section triplet for one window.
#[derive(Clone, Copy)]
pub struct WindowSections<'a> {
    pub data: &'a [u8],
    pub inst: &'a [u8],
    pub addr: &'a [u8],
}

/// Decodes a single VCDIFF window given the three sections and a source.
///
/// `copy_buf` is a reusable buffer for source COPY operations when zero-copy
/// slice access is not available. It is resized as needed and persists across
/// calls to avoid per-COPY allocations.
pub fn decode_window<S: SourceProvider>(
    header: &WindowHeader,
    data_section: &[u8],
    inst_section: &[u8],
    addr_section: &[u8],
    source: &mut S,
    verify_checksum: bool,
    copy_buf: &mut Vec<u8>,
) -> Result<Vec<u8>, DecodeError> {
    let target_len = header.target_window_len as usize;
    let mut output = Vec::with_capacity(target_len);
    decode_window_into(
        header,
        WindowSections {
            data: data_section,
            inst: inst_section,
            addr: addr_section,
        },
        source,
        verify_checksum,
        copy_buf,
        &mut output,
    )?;
    Ok(output)
}

/// Decodes a single VCDIFF window, appending output to `output`.
///
/// This avoids the intermediate Vec allocation that `decode_window` performs.
/// Target self-copy addresses are adjusted for the base offset in `output`.
pub fn decode_window_into<S: SourceProvider>(
    header: &WindowHeader,
    sections: WindowSections<'_>,
    source: &mut S,
    verify_checksum: bool,
    copy_buf: &mut Vec<u8>,
    output: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    let mut acache = AddressCache::new();
    decode_window_with_cache(
        header,
        sections.data,
        sections.inst,
        sections.addr,
        source,
        verify_checksum,
        copy_buf,
        output,
        &mut acache,
    )
}

/// Internal: decode a window using a reusable AddressCache (avoids re-allocation).
#[allow(clippy::too_many_arguments)]
fn decode_window_with_cache<S: SourceProvider>(
    header: &WindowHeader,
    data_section: &[u8],
    inst_section: &[u8],
    addr_section: &[u8],
    source: &mut S,
    verify_checksum: bool,
    copy_buf: &mut Vec<u8>,
    output: &mut Vec<u8>,
    acache: &mut AddressCache,
) -> Result<(), DecodeError> {
    let target_len = header.target_window_len as usize;
    let copy_window_len = header.copy_window_len;
    let copy_window_offset = header.copy_window_offset;

    // Base offset: self-copy addresses are relative to window start,
    // so we need to know where this window starts in the output buffer.
    let base_offset = output.len();
    output.reserve(target_len);

    acache.init();

    let mut data_pos: usize = 0;
    let mut inst_pos: usize = 0;
    let mut addr_pos: usize = 0;

    let code_table = code_table::default_code_table();

    // Current position in the target address space.
    let mut target_pos: u64 = 0;

    while inst_pos < inst_section.len() {
        let opcode = inst_section[inst_pos];
        inst_pos += 1;

        let entry = &code_table[opcode as usize];

        // Process first half-instruction.
        if entry.type1 != XD3_NOOP {
            execute_half_instruction(
                entry.type1,
                entry.size1,
                &mut inst_pos,
                inst_section,
                &mut data_pos,
                data_section,
                &mut addr_pos,
                addr_section,
                acache,
                copy_window_len,
                copy_window_offset,
                &mut target_pos,
                output,
                source,
                copy_buf,
                base_offset,
            )?;
        }

        // Process second half-instruction.
        if entry.type2 != XD3_NOOP {
            execute_half_instruction(
                entry.type2,
                entry.size2,
                &mut inst_pos,
                inst_section,
                &mut data_pos,
                data_section,
                &mut addr_pos,
                addr_section,
                acache,
                copy_window_len,
                copy_window_offset,
                &mut target_pos,
                output,
                source,
                copy_buf,
                base_offset,
            )?;
        }
    }

    // Validate target size.
    let written = output.len() - base_offset;
    if written as u64 != header.target_window_len {
        return Err(DecodeError::InvalidInput(format!(
            "target size mismatch: expected {}, got {}",
            header.target_window_len, written
        )));
    }

    // Validate checksum.
    if verify_checksum && let Some(expected) = header.adler32 {
        let actual = compute_adler32(&output[base_offset..]);
        if actual != expected {
            return Err(DecodeError::ChecksumMismatch { expected, actual });
        }
    }

    Ok(())
}

/// Execute a single half-instruction.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn execute_half_instruction<S: SourceProvider>(
    itype: u8,
    table_size: u8,
    inst_pos: &mut usize,
    inst_section: &[u8],
    data_pos: &mut usize,
    data_section: &[u8],
    addr_pos: &mut usize,
    addr_section: &[u8],
    acache: &mut AddressCache,
    copy_window_len: u64,
    copy_window_offset: u64,
    target_pos: &mut u64,
    output: &mut Vec<u8>,
    source: &mut S,
    copy_buf: &mut Vec<u8>,
    base_offset: usize,
) -> Result<(), DecodeError> {
    // Resolve size: if table_size==0, read from instruction section.
    let size = if table_size == 0 {
        let (val, consumed) = varint::read_u32(&inst_section[*inst_pos..])
            .map_err(|e| DecodeError::InvalidInput(format!("bad instruction size: {e}")))?;
        *inst_pos += consumed;
        val
    } else {
        table_size as u32
    };

    let size_usize = size as usize;

    match itype {
        XD3_RUN => {
            // Read 1 byte from data section, repeat `size` times.
            if *data_pos >= data_section.len() {
                return Err(DecodeError::InvalidInput(
                    "data section underflow (RUN)".into(),
                ));
            }
            let byte = data_section[*data_pos];
            *data_pos += 1;
            output.resize(output.len() + size_usize, byte);
            *target_pos += size as u64;
        }

        XD3_ADD => {
            // Read `size` bytes from data section.
            let end = *data_pos + size_usize;
            if end > data_section.len() {
                return Err(DecodeError::InvalidInput(
                    "data section underflow (ADD)".into(),
                ));
            }
            output.extend_from_slice(&data_section[*data_pos..end]);
            *data_pos += size_usize;
            *target_pos += size as u64;
        }

        _ => {
            // COPY: itype >= XD3_CPY, mode = itype - XD3_CPY
            let mode = itype - XD3_CPY;

            // Decode address.
            let here = copy_window_len + *target_pos;
            let (addr, consumed) = acache
                .decode(mode, &addr_section[*addr_pos..], here)
                .map_err(|e| DecodeError::InvalidInput(format!("address decode: {e}")))?;
            *addr_pos += consumed;

            // Validate: copy must not span source/target boundary.
            if addr < copy_window_len && addr + size as u64 > copy_window_len {
                return Err(DecodeError::InvalidInput(
                    "COPY spans source/target boundary".into(),
                ));
            }

            if addr < copy_window_len {
                // Source copy.
                let src_offset = copy_window_offset + addr;

                // Zero-copy fast path: use direct slice access when available.
                if let Some(slice) = source.source_slice(src_offset, size_usize) {
                    output.extend_from_slice(slice);
                } else {
                    // Fallback: use the reusable copy buffer.
                    copy_buf.resize(size_usize, 0);
                    let n = source.read_source(src_offset, copy_buf)?;
                    if n < size_usize {
                        return Err(DecodeError::InvalidInput(format!(
                            "source underflow: requested {size_usize} bytes at offset {src_offset}, got {n}"
                        )));
                    }
                    output.extend_from_slice(&copy_buf[..size_usize]);
                }
            } else {
                // Target self-copy.
                // Addresses in target space are relative to the current window.
                // Adjust by base_offset because `output` may already contain
                // previous windows.
                let tgt_offset = base_offset + (addr - copy_window_len) as usize;
                if tgt_offset + size_usize <= output.len() {
                    // Fast path: non-overlapping — use optimized bulk copy.
                    output.extend_from_within(tgt_offset..tgt_offset + size_usize);
                } else {
                    // Slow path: overlapping regions (RLE-like patterns where
                    // src and dst overlap). Must be byte-by-byte so reads see
                    // previously written output bytes.
                    for i in 0..size_usize {
                        let byte = output[tgt_offset + i];
                        output.push(byte);
                    }
                }
            }

            *target_pos += size as u64;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Stream decoder
// ---------------------------------------------------------------------------

/// Decodes a complete VCDIFF stream (file header + all windows).
///
/// Buffers are reused across windows to minimize allocations:
/// - Section buffers (data/inst/addr) grow to the largest section seen
/// - A copy buffer is reused across COPY instructions
pub struct StreamDecoder<R: Read> {
    reader: R,
    file_header: Option<FileHeader>,
    verify_checksum: bool,
    secondary_id: Option<u8>,
    /// Reusable section buffers (grow to largest section, never shrink).
    data_buf: Vec<u8>,
    inst_buf: Vec<u8>,
    addr_buf: Vec<u8>,
    /// Reusable buffer for source COPY (fallback when zero-copy unavailable).
    copy_buf: Vec<u8>,
    /// Reusable address cache (avoids re-allocation per window).
    acache: AddressCache,
}

impl<R: Read> StreamDecoder<R> {
    /// Create a new stream decoder.
    pub fn new(reader: R, verify_checksum: bool) -> Self {
        Self {
            reader,
            file_header: None,
            verify_checksum,
            secondary_id: None,
            data_buf: Vec::new(),
            inst_buf: Vec::new(),
            addr_buf: Vec::new(),
            copy_buf: Vec::new(),
            acache: AddressCache::new(),
        }
    }

    /// Read and return the file header.
    pub fn read_header(&mut self) -> Result<&FileHeader, DecodeError> {
        if self.file_header.is_none() {
            let hdr = FileHeader::decode(&mut self.reader)?;
            self.secondary_id = hdr.secondary_id;
            self.file_header = Some(hdr);
        }
        Ok(self.file_header.as_ref().unwrap())
    }

    /// The secondary compressor ID from the file header (if any).
    pub fn secondary_id(&self) -> Option<u8> {
        self.secondary_id
    }

    /// Decode the next window into `output`.
    /// Returns `Ok(false)` when there are no more windows.
    pub fn decode_window<S: SourceProvider>(
        &mut self,
        source: &mut S,
        output: &mut Vec<u8>,
    ) -> Result<bool, DecodeError> {
        // Ensure header is read.
        if self.file_header.is_none() {
            let hdr = FileHeader::decode(&mut self.reader)?;
            self.secondary_id = hdr.secondary_id;
            self.file_header = Some(hdr);
        }

        // Try to read the window header.
        let wh = match WindowHeader::decode(&mut self.reader)? {
            Some(wh) => wh,
            None => return Ok(false),
        };

        if wh.win_ind & VCD_TARGET != 0 {
            return Err(DecodeError::Unsupported("VCD_TARGET not supported".into()));
        }

        // Read sections into reusable buffers (resize, not re-allocate).
        self.data_buf.resize(wh.data_len as usize, 0);
        self.reader.read_exact(&mut self.data_buf)?;

        self.inst_buf.resize(wh.inst_len as usize, 0);
        self.reader.read_exact(&mut self.inst_buf)?;

        self.addr_buf.resize(wh.addr_len as usize, 0);
        self.reader.read_exact(&mut self.addr_buf)?;

        // Decompress sections if secondary compression is indicated.
        // Note: decompression produces new Vecs (unavoidable since the
        // decompressed size differs from compressed). The section bufs
        // still save allocations for the common non-secondary case.
        let (data_ref, inst_ref, addr_ref);
        let decomp_d;
        let decomp_i;
        let decomp_a;
        if wh.del_ind != 0 {
            let (d, i, a) = crate::compress::secondary::decompress_sections(
                &self.data_buf,
                &self.inst_buf,
                &self.addr_buf,
                wh.del_ind,
                self.secondary_id,
            )?;
            decomp_d = d;
            decomp_i = i;
            decomp_a = a;
            data_ref = &decomp_d[..];
            inst_ref = &decomp_i[..];
            addr_ref = &decomp_a[..];
        } else {
            data_ref = &self.data_buf;
            inst_ref = &self.inst_buf;
            addr_ref = &self.addr_buf;
        }

        // Decode the window directly into the output buffer (no intermediate Vec).
        // Reuse the address cache across windows to avoid re-allocation.
        decode_window_with_cache(
            &wh,
            data_ref,
            inst_ref,
            addr_ref,
            source,
            self.verify_checksum,
            &mut self.copy_buf,
            output,
            &mut self.acache,
        )?;

        Ok(true)
    }

    /// Decode all remaining windows, appending to `output`.
    pub fn decode_all<S: SourceProvider>(
        &mut self,
        source: &mut S,
        output: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        while self.decode_window(source, output)? {}
        Ok(())
    }

    /// Return the file header (panics if not yet read).
    pub fn file_header(&self) -> Option<&FileHeader> {
        self.file_header.as_ref()
    }
}

// ---------------------------------------------------------------------------
// High-level convenience: decode in memory
// ---------------------------------------------------------------------------

/// Decode a VCDIFF delta from memory.
///
/// `delta` is the complete VCDIFF-encoded byte stream.
/// `source` is the source/dictionary data (may be empty).
/// Returns the reconstructed target.
pub fn decode_memory(delta: &[u8], source: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = StreamDecoder::new(std::io::Cursor::new(delta), true);
    let mut output = Vec::new();
    let mut src: &[u8] = source;
    decoder.decode_all(&mut src, &mut output)?;
    Ok(output)
}

// ---------------------------------------------------------------------------
// Adler-32
// ---------------------------------------------------------------------------

fn compute_adler32(data: &[u8]) -> u32 {
    #[cfg(feature = "adler32")]
    {
        let mut hasher = simd_adler32::Adler32::new();
        hasher.write(data);
        hasher.finish()
    }
    #[cfg(not(feature = "adler32"))]
    {
        const MOD_ADLER: u32 = 65521;
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + u32::from(byte)) % MOD_ADLER;
            b = (b + a) % MOD_ADLER;
        }
        (b << 16) | a
    }
}

// ---------------------------------------------------------------------------
// Instruction iterator (for inspection/debugging)
// ---------------------------------------------------------------------------

/// Iterate over decoded instructions in a window's instruction section.
pub struct InstructionIterator<'a> {
    inst_data: &'a [u8],
    addr_data: &'a [u8],
    inst_pos: usize,
    addr_pos: usize,
    code_table: &'static CodeTable,
    acache: AddressCache,
    copy_window_len: u64,
    target_pos: u64,
    /// Buffered second instruction from a double opcode.
    pending_second: Option<(u8, u8)>,
}

impl<'a> InstructionIterator<'a> {
    pub fn new(inst_section: &'a [u8], addr_section: &'a [u8], copy_window_len: u64) -> Self {
        Self {
            inst_data: inst_section,
            addr_data: addr_section,
            inst_pos: 0,
            addr_pos: 0,
            code_table: code_table::default_code_table(),
            acache: AddressCache::new(),
            copy_window_len,
            target_pos: 0,
            pending_second: None,
        }
    }

    fn resolve_half(
        &mut self,
        itype: u8,
        table_size: u8,
    ) -> Result<Option<Instruction>, DecodeError> {
        if itype == XD3_NOOP {
            return Ok(None);
        }

        let size = if table_size == 0 {
            let (val, consumed) = varint::read_u32(&self.inst_data[self.inst_pos..])
                .map_err(|e| DecodeError::InvalidInput(format!("size varint: {e}")))?;
            self.inst_pos += consumed;
            val
        } else {
            table_size as u32
        };

        let inst = match itype {
            XD3_RUN => Instruction::Run { len: size },
            XD3_ADD => Instruction::Add { len: size },
            _ => {
                let mode = itype - XD3_CPY;
                let here = self.copy_window_len + self.target_pos;
                let (addr, consumed) = self
                    .acache
                    .decode(mode, &self.addr_data[self.addr_pos..], here)
                    .map_err(|e| DecodeError::InvalidInput(format!("address: {e}")))?;
                self.addr_pos += consumed;
                Instruction::Copy {
                    len: size,
                    addr,
                    mode,
                }
            }
        };

        self.target_pos += size as u64;
        Ok(Some(inst))
    }
}

impl Iterator for InstructionIterator<'_> {
    type Item = Result<Instruction, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        // First, drain any pending second half-instruction.
        if let Some((type2, size2)) = self.pending_second.take() {
            return match self.resolve_half(type2, size2) {
                Ok(Some(inst)) => Some(Ok(inst)),
                Ok(None) => self.next(),
                Err(e) => Some(Err(e)),
            };
        }

        if self.inst_pos >= self.inst_data.len() {
            return None;
        }

        let opcode = self.inst_data[self.inst_pos];
        self.inst_pos += 1;
        let entry = &self.code_table[opcode as usize];

        // Buffer second half.
        if entry.type2 != XD3_NOOP {
            self.pending_second = Some((entry.type2, entry.size2));
        }

        match self.resolve_half(entry.type1, entry.size1) {
            Ok(Some(inst)) => Some(Ok(inst)),
            Ok(None) => self.next(),
            Err(e) => Some(Err(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcdiff::encoder::{SourceWindow, StreamEncoder, WindowEncoder};

    /// Helper: encode instructions into a VCDIFF stream and decode it back.
    fn roundtrip_instructions(
        instructions: &[Instruction],
        source: &[u8],
        target: &[u8],
    ) -> Vec<u8> {
        let src_win = if source.is_empty() {
            None
        } else {
            Some(SourceWindow {
                len: source.len() as u64,
                offset: 0,
            })
        };

        let mut we = WindowEncoder::new(src_win, true);
        let mut data_offset: usize = 0;

        for inst in instructions {
            match *inst {
                Instruction::Add { len } => {
                    we.add(&target[data_offset..data_offset + len as usize]);
                    data_offset += len as usize;
                }
                Instruction::Copy { len, addr, .. } => {
                    we.copy_with_auto_mode(len, addr);
                    data_offset += len as usize;
                }
                Instruction::Run { len } => {
                    we.run(len, target[data_offset]);
                    data_offset += len as usize;
                }
            }
        }

        let mut out = Vec::new();
        let mut enc = StreamEncoder::new(&mut out, true);
        enc.write_window(we, Some(target)).unwrap();
        let _ = enc.finish().unwrap();
        out
    }

    #[test]
    fn decode_simple_add() {
        let target = b"Hello, world!";
        let instructions = vec![Instruction::Add {
            len: target.len() as u32,
        }];
        let delta = roundtrip_instructions(&instructions, &[], target);
        let result = decode_memory(&delta, &[]).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn decode_simple_run() {
        let target = vec![0xAA; 50];
        let instructions = vec![Instruction::Run { len: 50 }];
        let delta = roundtrip_instructions(&instructions, &[], &target);
        let result = decode_memory(&delta, &[]).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn decode_source_copy() {
        let source = b"ABCDEFGHIJKLMNOP";
        let target = &source[4..12]; // "EFGHIJKL"
        let instructions = vec![Instruction::Copy {
            len: 8,
            addr: 4,
            mode: 0,
        }];
        let delta = roundtrip_instructions(&instructions, source, target);
        let result = decode_memory(&delta, source).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn decode_mixed_instructions() {
        let source = b"The quick brown fox";
        // Target: "Hello" + copy("quick") + " world"
        let target = b"Helloquick world";
        let instructions = vec![
            Instruction::Add { len: 5 }, // "Hello"
            Instruction::Copy {
                len: 5,
                addr: 4,
                mode: 0,
            }, // "quick"
            Instruction::Add { len: 6 }, // " world"
        ];
        let delta = roundtrip_instructions(&instructions, source, target);
        let result = decode_memory(&delta, source).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn decode_target_self_copy() {
        let target = b"ABCDABCD";
        let instructions = vec![
            Instruction::Add { len: 4 },
            Instruction::Copy {
                len: 4,
                addr: 0,
                mode: 0,
            },
        ];
        let delta = roundtrip_instructions(&instructions, &[], target);
        let result = decode_memory(&delta, &[]).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn decode_overlapping_self_copy() {
        let target = b"AAAAAA";
        let instructions = vec![
            Instruction::Add { len: 1 },
            Instruction::Copy {
                len: 5,
                addr: 0,
                mode: 0,
            },
        ];
        let delta = roundtrip_instructions(&instructions, &[], target);
        let result = decode_memory(&delta, &[]).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn checksum_verification() {
        let target = b"test data for checksum";
        let instructions = vec![Instruction::Add {
            len: target.len() as u32,
        }];
        let delta = roundtrip_instructions(&instructions, &[], target);
        let result = decode_memory(&delta, &[]).unwrap();
        assert_eq!(result, target);
    }

    #[test]
    fn instruction_iterator_basic() {
        let target = b"Hello, world!";
        let instructions = vec![Instruction::Add {
            len: target.len() as u32,
        }];
        let delta = roundtrip_instructions(&instructions, &[], target);

        let mut cursor = std::io::Cursor::new(&delta);
        let _fh = FileHeader::decode(&mut cursor).unwrap();
        let wh = WindowHeader::decode(&mut cursor).unwrap().unwrap();

        let mut data_sec = vec![0u8; wh.data_len as usize];
        cursor.read_exact(&mut data_sec).unwrap();
        let mut inst_sec = vec![0u8; wh.inst_len as usize];
        cursor.read_exact(&mut inst_sec).unwrap();
        let mut addr_sec = vec![0u8; wh.addr_len as usize];
        cursor.read_exact(&mut addr_sec).unwrap();

        let iter = InstructionIterator::new(&inst_sec, &addr_sec, 0);
        let decoded: Vec<_> = iter.collect::<Result<_, _>>().unwrap();
        assert_eq!(decoded.len(), 1);
        match decoded[0] {
            Instruction::Add { len } => assert_eq!(len, target.len() as u32),
            _ => panic!("expected Add instruction"),
        }
    }

    #[test]
    fn zero_copy_source_slice() {
        let source = b"ABCDEFGHIJKLMNOP";
        // Verify source_slice returns the correct data.
        let src: &[u8] = source;
        assert_eq!(src.source_slice(4, 8), Some(b"EFGHIJKL".as_slice()));
        assert_eq!(src.source_slice(0, 16), Some(source.as_slice()));
        assert_eq!(src.source_slice(15, 2), None); // out of bounds
        assert_eq!(src.source_slice(0, 0), Some(b"".as_slice()));
    }

    #[test]
    fn reusable_buffers_in_stream_decoder() {
        // Encode two windows.
        let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let target1 = b"ABCDEFGH_changed";
        let target2 = b"MNOPQRST_different";

        let mut delta = Vec::new();
        let mut enc = crate::vcdiff::encoder::StreamEncoder::new(&mut delta, true);
        {
            let src_win = SourceWindow {
                len: source.len() as u64,
                offset: 0,
            };
            let mut we = WindowEncoder::new(Some(src_win), true);
            let mut pos = 0usize;
            we.add(target1);
            pos += target1.len();
            let _ = pos;
            enc.write_window(we, Some(target1)).unwrap();
        }
        {
            let src_win = SourceWindow {
                len: source.len() as u64,
                offset: 0,
            };
            let mut we = WindowEncoder::new(Some(src_win), true);
            we.add(target2);
            enc.write_window(we, Some(target2)).unwrap();
        }
        let _ = enc.finish().unwrap();

        // Decode both windows — buffers should be reused.
        let mut decoder = StreamDecoder::new(std::io::Cursor::new(&delta), true);
        let mut src: &[u8] = source;
        let mut output = Vec::new();
        decoder.decode_all(&mut src, &mut output).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(target1);
        expected.extend_from_slice(target2);
        assert_eq!(output, expected);
    }
}
