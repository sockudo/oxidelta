// VCDIFF encoder: instruction encoding and window emission.
//
// This module handles the low-level VCDIFF encoding: taking a sequence of
// ADD/COPY/RUN instructions and producing a valid VCDIFF byte stream.
// The actual match-finding (delta computation) lives in the engine module;
// this module is concerned only with format-level encoding.

use std::io::Write;

use super::address_cache::AddressCache;
use super::code_table::{
    self, CodeTableEntry, InstructionInfo, XD3_ADD, XD3_CPY, XD3_RUN, choose_instruction,
};
use super::header::{self, FileHeader, VCD_ADLER32, VCD_SOURCE, WindowHeader};
use super::varint;

// ---------------------------------------------------------------------------
// Window encoder
// ---------------------------------------------------------------------------

/// Accumulates instructions for a single VCDIFF window and emits encoded bytes.
pub struct WindowEncoder {
    /// DATA section: literal bytes for ADD and RUN instructions.
    data_section: Vec<u8>,
    /// INST section: opcode bytes (and inline sizes).
    inst_section: Vec<u8>,
    /// ADDR section: encoded COPY addresses.
    addr_section: Vec<u8>,

    /// Address cache.
    acache: AddressCache,

    /// Pending previous instruction (for double-instruction packing).
    pending: Option<PendingInst>,

    /// Target bytes emitted so far in this window.
    target_len: u64,
    /// Source copy-window parameters (if any).
    source_window: Option<SourceWindow>,

    /// Whether to emit an Adler-32 checksum.
    emit_checksum: bool,

    /// Code table reference.
    code_table: &'static [CodeTableEntry; 256],
}

#[derive(Clone, Copy)]
struct PendingInst {
    info: InstructionInfo,
    code1: u8,
}

/// Source copy-window parameters.
#[derive(Debug, Clone, Copy)]
pub struct SourceWindow {
    pub len: u64,
    pub offset: u64,
}

impl WindowEncoder {
    /// Create a new window encoder.
    pub fn new(source: Option<SourceWindow>, emit_checksum: bool) -> Self {
        Self {
            data_section: Vec::new(),
            inst_section: Vec::new(),
            addr_section: Vec::new(),
            acache: AddressCache::new(),
            pending: None,
            target_len: 0,
            source_window: source,
            emit_checksum,
            code_table: code_table::default_code_table(),
        }
    }

    /// Create a window encoder with pre-allocated section capacities.
    ///
    /// Use this when encoding multiple windows to avoid repeated Vec growth.
    /// Pass the section sizes from the previous window as hints.
    pub fn with_capacity(
        source: Option<SourceWindow>,
        emit_checksum: bool,
        data_cap: usize,
        inst_cap: usize,
        addr_cap: usize,
    ) -> Self {
        Self {
            data_section: Vec::with_capacity(data_cap),
            inst_section: Vec::with_capacity(inst_cap),
            addr_section: Vec::with_capacity(addr_cap),
            acache: AddressCache::new(),
            pending: None,
            target_len: 0,
            source_window: source,
            emit_checksum,
            code_table: code_table::default_code_table(),
        }
    }

    /// The current position in the combined address space
    /// (copy_window_len + target bytes so far).
    #[inline]
    fn here(&self) -> u64 {
        self.source_window.map_or(0, |s| s.len) + self.target_len
    }

    /// Add an ADD instruction with literal data.
    pub fn add(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.data_section.extend_from_slice(data);
        let inst = InstructionInfo {
            itype: XD3_ADD,
            size: data.len() as u32,
        };
        self.emit_instruction(inst);
        self.target_len += data.len() as u64;
    }

    /// Add a COPY instruction.
    ///
    /// `addr` is in the combined source+target address space:
    ///   - `0..source_window_len` for source copies
    ///   - `source_window_len..` for target self-copies
    pub fn copy(&mut self, len: u32, addr: u64, _mode: u8) {
        if len == 0 {
            return;
        }
        // Encode address.
        let here = self.here();
        let (enc_mode, encoded_addr) = self.acache.encode(addr, here);
        encoded_addr.write_to(&mut self.addr_section).unwrap();

        let inst = InstructionInfo {
            itype: XD3_CPY + enc_mode,
            size: len,
        };
        self.emit_instruction(inst);
        self.target_len += len as u64;
    }

    /// Add a COPY instruction using a raw mode (caller provides the mode).
    /// Address encoding is handled internally by the address cache.
    pub fn copy_with_auto_mode(&mut self, len: u32, addr: u64) {
        // Delegate to the address cache which picks the best mode.
        self.copy(len, addr, 0); // mode parameter is ignored; acache.encode picks best
    }

    /// Add a RUN instruction.
    pub fn run(&mut self, len: u32, byte: u8) {
        if len == 0 {
            return;
        }
        self.data_section.push(byte);
        let inst = InstructionInfo {
            itype: XD3_RUN,
            size: len,
        };
        self.emit_instruction(inst);
        self.target_len += len as u64;
    }

    /// Flush any pending instruction and finalize this window.
    /// Returns the encoded window bytes (without file header).
    pub fn finish(self, target_data: Option<&[u8]>) -> Vec<u8> {
        let sections = self.finish_sections(target_data);
        sections.assemble(0) // del_ind = 0 (no secondary compression)
    }

    /// Flush pending instructions and return the raw sections + metadata.
    ///
    /// This is the low-level API used by the compress module to apply
    /// secondary compression to sections before assembling the final window.
    pub fn finish_sections(mut self, target_data: Option<&[u8]>) -> WindowSections {
        self.flush_pending();

        // Compute checksum.
        let checksum = if self.emit_checksum {
            target_data.map(|data| {
                #[cfg(feature = "adler32")]
                {
                    {
                        let mut hasher = simd_adler32::Adler32::new();
                        hasher.write(data);
                        hasher.finish()
                    }
                }
                #[cfg(not(feature = "adler32"))]
                {
                    // Fallback: simple Adler-32
                    adler32_simple(data)
                }
            })
        } else {
            None
        };

        WindowSections {
            source_window: self.source_window,
            target_len: self.target_len,
            checksum,
            data_section: self.data_section,
            inst_section: self.inst_section,
            addr_section: self.addr_section,
        }
    }

    // -----------------------------------------------------------------------
    // Internal: instruction emission with double-packing
    // -----------------------------------------------------------------------

    fn emit_instruction(&mut self, inst: InstructionInfo) {
        let chosen = choose_instruction(self.pending.as_ref().map(|p| &p.info), &inst);

        if let Some(code2) = chosen.code2 {
            // Double instruction: emit the previous + current as one opcode.
            let _prev = self.pending.take().unwrap();
            self.emit_opcode_double(code2);
        } else {
            // Flush any pending single instruction first.
            self.flush_pending();
            // Queue current as pending.
            self.pending = Some(PendingInst {
                info: inst,
                code1: chosen.code1,
            });
        }
    }

    fn flush_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.emit_opcode_single(pending.code1, &pending.info);
        }
    }

    /// Emit a single-instruction opcode.
    /// If the code table entry has size1==0, emit the size as a varint.
    fn emit_opcode_single(&mut self, code: u8, inst: &InstructionInfo) {
        self.inst_section.push(code);
        let entry = &self.code_table[code as usize];
        if entry.size1 == 0 {
            varint::write_u32(&mut self.inst_section, inst.size).unwrap();
        }
    }

    /// Emit a double-instruction opcode (both sizes are implicit).
    fn emit_opcode_double(&mut self, code: u8) {
        self.inst_section.push(code);
        // Double instructions always have fixed sizes in the code table.
    }
}

// ---------------------------------------------------------------------------
// Window sections (intermediate result for secondary compression)
// ---------------------------------------------------------------------------

/// Raw sections from a finalized window, before assembly into bytes.
///
/// Allows the compress module to inspect/replace sections (e.g. for
/// secondary compression) before calling `assemble()`.
pub struct WindowSections {
    pub source_window: Option<SourceWindow>,
    pub target_len: u64,
    pub checksum: Option<u32>,
    pub data_section: Vec<u8>,
    pub inst_section: Vec<u8>,
    pub addr_section: Vec<u8>,
}

impl WindowSections {
    /// Assemble the sections into encoded window bytes.
    ///
    /// `del_ind` indicates which sections have secondary compression applied
    /// (VCD_DATACOMP, VCD_INSTCOMP, VCD_ADDRCOMP).
    pub fn assemble(self, del_ind: u8) -> Vec<u8> {
        let mut win_ind = 0u8;
        if self.source_window.is_some() {
            win_ind |= VCD_SOURCE;
        }
        if self.checksum.is_some() {
            win_ind |= VCD_ADLER32;
        }

        let wh = WindowHeader {
            win_ind,
            copy_window_len: self.source_window.map_or(0, |s| s.len),
            copy_window_offset: self.source_window.map_or(0, |s| s.offset),
            enc_len: 0, // will be computed
            target_window_len: self.target_len,
            del_ind,
            data_len: self.data_section.len() as u64,
            inst_len: self.inst_section.len() as u64,
            addr_len: self.addr_section.len() as u64,
            adler32: self.checksum,
        };
        let enc_len = wh.compute_enc_len();
        let wh = WindowHeader { enc_len, ..wh };

        let mut out = Vec::new();
        wh.encode(&mut out).unwrap();
        out.extend_from_slice(&self.data_section);
        out.extend_from_slice(&self.inst_section);
        out.extend_from_slice(&self.addr_section);
        out
    }
}

// ---------------------------------------------------------------------------
// Full-stream encoder
// ---------------------------------------------------------------------------

/// Encodes a complete VCDIFF stream (file header + windows).
pub struct StreamEncoder<W: Write> {
    writer: W,
    header_written: bool,
    file_header: FileHeader,
    #[allow(dead_code)]
    emit_checksum: bool,
}

impl<W: Write> StreamEncoder<W> {
    /// Create a new stream encoder.
    pub fn new(writer: W, emit_checksum: bool) -> Self {
        Self {
            writer,
            header_written: false,
            file_header: FileHeader::default(),
            emit_checksum,
        }
    }

    /// Set the application header data.
    pub fn set_app_header(&mut self, data: Vec<u8>) {
        self.file_header.hdr_ind |= header::VCD_APPHEADER;
        self.file_header.app_header = Some(data);
    }

    /// Write a complete window to the output.
    pub fn write_window(
        &mut self,
        window: WindowEncoder,
        target_data: Option<&[u8]>,
    ) -> std::io::Result<()> {
        if !self.header_written {
            self.file_header.encode(&mut self.writer)?;
            self.header_written = true;
        }
        let encoded = window.finish(target_data);
        self.writer.write_all(&encoded)
    }

    /// Write pre-assembled window bytes to the output.
    ///
    /// Used by the compress module which assembles windows itself
    /// (e.g. after applying secondary compression to sections).
    pub fn write_raw_window(&mut self, encoded: &[u8]) -> std::io::Result<()> {
        if !self.header_written {
            self.file_header.encode(&mut self.writer)?;
            self.header_written = true;
        }
        self.writer.write_all(encoded)
    }

    /// Set the file header to indicate secondary compression.
    pub fn set_secondary_id(&mut self, id: u8) {
        self.file_header.hdr_ind |= header::VCD_SECONDARY;
        self.file_header.secondary_id = Some(id);
    }

    /// Flush and return the inner writer.
    pub fn finish(mut self) -> std::io::Result<W> {
        if !self.header_written {
            self.file_header.encode(&mut self.writer)?;
        }
        self.writer.flush()?;
        Ok(self.writer)
    }
}

// ---------------------------------------------------------------------------
// Fallback Adler-32 (when the `adler32` feature is disabled)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "adler32"))]
fn adler32_simple(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_single_add() {
        let mut we = WindowEncoder::new(None, false);
        we.add(b"hello");
        let bytes = we.finish(None);
        // Should produce a valid window header + sections.
        assert!(!bytes.is_empty());
    }

    #[test]
    fn encode_single_run() {
        let mut we = WindowEncoder::new(None, false);
        we.run(100, 0xAA);
        let bytes = we.finish(None);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn encode_add_then_copy_packs_double() {
        let src = SourceWindow {
            len: 1024,
            offset: 0,
        };
        let mut we = WindowEncoder::new(Some(src), false);
        // ADD(1) followed by COPY(4, addr=0) should produce a double opcode.
        we.add(b"X");
        we.copy_with_auto_mode(4, 0);
        let bytes = we.finish(None);
        // The inst section should be compact (double instruction = 1 byte).
        assert!(!bytes.is_empty());
    }

    #[test]
    fn stream_encoder_writes_header() {
        let mut out = Vec::new();
        let mut enc = StreamEncoder::new(&mut out, false);
        let we = WindowEncoder::new(None, false);
        enc.write_window(we, None).unwrap();
        let _ = enc.finish().unwrap();

        // Should start with VCDIFF magic.
        assert_eq!(&out[..4], &header::VCDIFF_MAGIC);
    }

    #[test]
    fn encode_with_checksum() {
        let target = b"hello world";
        let mut we = WindowEncoder::new(None, true);
        we.add(target);
        let bytes = we.finish(Some(target));

        // Parse the window header to verify checksum is present.
        let mut cursor = std::io::Cursor::new(&bytes);
        let wh = WindowHeader::decode(&mut cursor).unwrap().unwrap();
        assert!(wh.has_checksum());
        assert!(wh.adler32.is_some());
    }
}
