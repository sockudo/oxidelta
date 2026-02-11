// VCDIFF file header and per-window header encoding/decoding (RFC 3284).
//
// Byte-for-byte compatible with xdelta3's header emission and parsing.

use std::io::{self, Read, Write};

use super::varint;

// ---------------------------------------------------------------------------
// VCDIFF magic and version
// ---------------------------------------------------------------------------

pub const VCDIFF_MAGIC: [u8; 4] = [0xD6, 0xC3, 0xC4, 0x00];

// ---------------------------------------------------------------------------
// Header indicator flags (hdr_ind)
// ---------------------------------------------------------------------------

pub const VCD_SECONDARY: u8 = 1 << 0;
pub const VCD_CODETABLE: u8 = 1 << 1;
pub const VCD_APPHEADER: u8 = 1 << 2;
/// Mask for invalid header indicator bits.
pub const VCD_INVHDR: u8 = !0x07;

// ---------------------------------------------------------------------------
// Window indicator flags (win_ind)
// ---------------------------------------------------------------------------

pub const VCD_SOURCE: u8 = 1 << 0;
pub const VCD_TARGET: u8 = 1 << 1;
pub const VCD_ADLER32: u8 = 1 << 2;
/// Mask for invalid window indicator bits.
pub const VCD_INVWIN: u8 = !0x07;

// ---------------------------------------------------------------------------
// Delta indicator flags (del_ind)
// ---------------------------------------------------------------------------

pub const VCD_DATACOMP: u8 = 1 << 0;
pub const VCD_INSTCOMP: u8 = 1 << 1;
pub const VCD_ADDRCOMP: u8 = 1 << 2;
/// Mask for invalid delta indicator bits.
pub const VCD_INVDEL: u8 = !0x07;

// ---------------------------------------------------------------------------
// Secondary compressor IDs
// ---------------------------------------------------------------------------

pub const VCD_DJW_ID: u8 = 1;
pub const VCD_LZMA_ID: u8 = 2;
pub const VCD_FGK_ID: u8 = 16;

// ---------------------------------------------------------------------------
// Hard limits
// ---------------------------------------------------------------------------

/// Maximum decoded window size (matches xdelta3 XD3_HARDMAXWINSIZE).
pub const HARD_MAX_WINSIZE: u64 = 1 << 24; // 16 MiB

// ---------------------------------------------------------------------------
// File header
// ---------------------------------------------------------------------------

/// Parsed VCDIFF file header.
#[derive(Debug, Clone, Default)]
pub struct FileHeader {
    /// Header indicator byte.
    pub hdr_ind: u8,
    /// Secondary compressor ID (if VCD_SECONDARY is set).
    pub secondary_id: Option<u8>,
    /// Application-defined header data (if VCD_APPHEADER is set).
    pub app_header: Option<Vec<u8>>,
}

impl FileHeader {
    /// Encode the file header to a writer.
    ///
    /// Matches xdelta3's header emission order:
    /// 1. Magic (4 bytes)
    /// 2. hdr_ind (1 byte)
    /// 3. [secondary_id] (1 byte, if VCD_SECONDARY)
    /// 4. [app_header_len + app_header_data] (if VCD_APPHEADER)
    pub fn encode<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&VCDIFF_MAGIC)?;
        w.write_all(&[self.hdr_ind])?;

        if self.hdr_ind & VCD_SECONDARY != 0 {
            let id = self.secondary_id.unwrap_or(0);
            w.write_all(&[id])?;
        }

        // VCD_CODETABLE is not supported (matches xdelta3).

        if self.hdr_ind & VCD_APPHEADER != 0 {
            if let Some(ref data) = self.app_header {
                varint::write_usize(w, data.len())?;
                w.write_all(data)?;
            } else {
                varint::write_usize(w, 0)?;
            }
        }

        Ok(())
    }

    /// Decode a VCDIFF file header from a reader.
    ///
    /// Matches xdelta3's decoder states DEC_VCHEAD through DEC_APPDAT.
    pub fn decode<R: Read>(r: &mut R) -> io::Result<Self> {
        // DEC_VCHEAD: read and validate magic bytes.
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if magic[..3] != VCDIFF_MAGIC[..3] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid VCDIFF magic: expected {:02X} {:02X} {:02X}, got {:02X} {:02X} {:02X}",
                    VCDIFF_MAGIC[0], VCDIFF_MAGIC[1], VCDIFF_MAGIC[2], magic[0], magic[1], magic[2]
                ),
            ));
        }
        if magic[3] != 0x00 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported VCDIFF version: {:#04X}", magic[3]),
            ));
        }

        // DEC_HDRIND
        let mut buf1 = [0u8; 1];
        r.read_exact(&mut buf1)?;
        let hdr_ind = buf1[0];
        if hdr_ind & VCD_INVHDR != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid header indicator bits: {hdr_ind:#04X}"),
            ));
        }

        // DEC_SECONDID
        let secondary_id = if hdr_ind & VCD_SECONDARY != 0 {
            r.read_exact(&mut buf1)?;
            Some(buf1[0])
        } else {
            None
        };

        // DEC_TABLEN / DEC_NEAR / DEC_SAME / DEC_TABDAT
        if hdr_ind & VCD_CODETABLE != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VCD_CODETABLE is not supported",
            ));
        }

        // DEC_APPLEN / DEC_APPDAT
        let app_header = if hdr_ind & VCD_APPHEADER != 0 {
            let len = varint::stream_read_usize(r)?;
            let mut data = vec![0u8; len];
            r.read_exact(&mut data)?;
            Some(data)
        } else {
            None
        };

        Ok(Self {
            hdr_ind,
            secondary_id,
            app_header,
        })
    }
}

// ---------------------------------------------------------------------------
// Per-window header
// ---------------------------------------------------------------------------

/// Parsed VCDIFF per-window header.
#[derive(Debug, Clone, Default)]
pub struct WindowHeader {
    /// Window indicator byte.
    pub win_ind: u8,

    // --- Copy window (if VCD_SOURCE or VCD_TARGET) ---
    /// Length of the source/target copy window.
    pub copy_window_len: u64,
    /// Offset into the source/target for the copy window.
    pub copy_window_offset: u64,

    // --- Delta encoding lengths ---
    /// Total length of the delta encoding (redundancy check field).
    pub enc_len: u64,
    /// Length of the target window to reconstruct.
    pub target_window_len: u64,
    /// Delta indicator (secondary compression flags).
    pub del_ind: u8,

    // --- Section sizes ---
    /// Length of the data section.
    pub data_len: u64,
    /// Length of the instruction section.
    pub inst_len: u64,
    /// Length of the address section.
    pub addr_len: u64,

    // --- Optional checksum ---
    /// Adler-32 checksum of the target window (if VCD_ADLER32).
    pub adler32: Option<u32>,
}

impl WindowHeader {
    /// Is this a source-copy window?
    #[inline]
    pub fn has_source(&self) -> bool {
        self.win_ind & VCD_SOURCE != 0
    }

    /// Is this a target-copy window?
    #[inline]
    pub fn has_target(&self) -> bool {
        self.win_ind & VCD_TARGET != 0
    }

    /// Is the Adler-32 checksum present?
    #[inline]
    pub fn has_checksum(&self) -> bool {
        self.win_ind & VCD_ADLER32 != 0
    }

    /// Encode a per-window header.
    ///
    /// Layout (matches xdelta3 `xd3_emit_hdr` per-window section):
    /// 1. win_ind
    /// 2. [copy_window_len, copy_window_offset] if source/target
    /// 3. enc_len (varint)
    /// 4. target_window_len (varint)
    /// 5. del_ind (1 byte)
    /// 6. data_len, inst_len, addr_len (varints)
    /// 7. [adler32] (4 bytes, big-endian) if VCD_ADLER32
    pub fn encode<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[self.win_ind])?;

        if self.has_source() || self.has_target() {
            varint::write_u64(w, self.copy_window_len)?;
            varint::write_u64(w, self.copy_window_offset)?;
        }

        varint::write_u64(w, self.enc_len)?;
        varint::write_u64(w, self.target_window_len)?;
        w.write_all(&[self.del_ind])?;
        varint::write_u64(w, self.data_len)?;
        varint::write_u64(w, self.inst_len)?;
        varint::write_u64(w, self.addr_len)?;

        if self.has_checksum()
            && let Some(cksum) = self.adler32
        {
            let bytes = cksum.to_be_bytes();
            w.write_all(&bytes)?;
        }

        Ok(())
    }

    /// Compute the expected `enc_len` from the current field values.
    ///
    /// `enc_len` is a redundancy check: it equals
    ///   sizeof(target_window_len) + 1(del_ind) +
    ///   sizeof(data_len) + sizeof(inst_len) + sizeof(addr_len) +
    ///   data_len + inst_len + addr_len +
    ///   [4 if adler32]
    pub fn compute_enc_len(&self) -> u64 {
        let mut len = 0u64;
        len += varint::sizeof_u64(self.target_window_len) as u64;
        len += 1; // del_ind
        len += varint::sizeof_u64(self.data_len) as u64;
        len += varint::sizeof_u64(self.inst_len) as u64;
        len += varint::sizeof_u64(self.addr_len) as u64;
        len += self.data_len;
        len += self.inst_len;
        len += self.addr_len;
        if self.has_checksum() {
            len += 4;
        }
        len
    }

    /// Decode a per-window header.
    ///
    /// Matches xdelta3 decoder states DEC_WININD through DEC_CKSUM.
    /// Returns `None` on clean EOF (no more windows).
    pub fn decode<R: Read>(r: &mut R) -> io::Result<Option<Self>> {
        // DEC_WININD
        let mut buf1 = [0u8; 1];
        match r.read_exact(&mut buf1) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let win_ind = buf1[0];
        if win_ind & VCD_INVWIN != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid window indicator bits: {win_ind:#04X}"),
            ));
        }

        let has_copy = win_ind & (VCD_SOURCE | VCD_TARGET) != 0;
        if win_ind & VCD_SOURCE != 0 && win_ind & VCD_TARGET != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VCD_SOURCE and VCD_TARGET are mutually exclusive",
            ));
        }

        // DEC_CPYLEN / DEC_CPYOFF
        let (copy_window_len, copy_window_offset) = if has_copy {
            let len = varint::stream_read_u64(r)?;
            let off = varint::stream_read_u64(r)?;
            (len, off)
        } else {
            (0, 0)
        };

        // DEC_ENCLEN
        let enc_len = varint::stream_read_u64(r)?;

        // DEC_TGTLEN
        let target_window_len = varint::stream_read_u64(r)?;

        // Hard-limit check.
        if target_window_len > HARD_MAX_WINSIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "target window too large: {} exceeds max {}",
                    target_window_len, HARD_MAX_WINSIZE
                ),
            ));
        }

        // DEC_DELIND
        r.read_exact(&mut buf1)?;
        let del_ind = buf1[0];
        if del_ind & VCD_INVDEL != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid delta indicator bits: {del_ind:#04X}"),
            ));
        }

        // DEC_DATALEN / DEC_INSTLEN / DEC_ADDRLEN
        let data_len = varint::stream_read_u64(r)?;
        let inst_len = varint::stream_read_u64(r)?;
        let addr_len = varint::stream_read_u64(r)?;

        // DEC_CKSUM
        let adler32 = if win_ind & VCD_ADLER32 != 0 {
            let mut cksum_buf = [0u8; 4];
            r.read_exact(&mut cksum_buf)?;
            Some(u32::from_be_bytes(cksum_buf))
        } else {
            None
        };

        let hdr = WindowHeader {
            win_ind,
            copy_window_len,
            copy_window_offset,
            enc_len,
            target_window_len,
            del_ind,
            data_len,
            inst_len,
            addr_len,
            adler32,
        };

        // Redundancy check.
        let expected = hdr.compute_enc_len();
        if enc_len != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("enc_len mismatch: header says {enc_len}, computed {expected}"),
            ));
        }

        Ok(Some(hdr))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn file_header_roundtrip_minimal() {
        let hdr = FileHeader {
            hdr_ind: 0,
            secondary_id: None,
            app_header: None,
        };
        let mut buf = Vec::new();
        hdr.encode(&mut buf).unwrap();
        assert_eq!(&buf[..4], &VCDIFF_MAGIC);
        assert_eq!(buf[4], 0);

        let decoded = FileHeader::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.hdr_ind, 0);
        assert!(decoded.secondary_id.is_none());
        assert!(decoded.app_header.is_none());
    }

    #[test]
    fn file_header_roundtrip_with_appheader() {
        let hdr = FileHeader {
            hdr_ind: VCD_APPHEADER,
            secondary_id: None,
            app_header: Some(b"xdelta test".to_vec()),
        };
        let mut buf = Vec::new();
        hdr.encode(&mut buf).unwrap();

        let decoded = FileHeader::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.hdr_ind, VCD_APPHEADER);
        assert_eq!(
            decoded.app_header.as_deref(),
            Some(b"xdelta test".as_slice())
        );
    }

    #[test]
    fn file_header_roundtrip_with_secondary() {
        let hdr = FileHeader {
            hdr_ind: VCD_SECONDARY,
            secondary_id: Some(VCD_LZMA_ID),
            app_header: None,
        };
        let mut buf = Vec::new();
        hdr.encode(&mut buf).unwrap();

        let decoded = FileHeader::decode(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.secondary_id, Some(VCD_LZMA_ID));
    }

    #[test]
    fn file_header_rejects_bad_magic() {
        let data = [0x00, 0x00, 0x00, 0x00, 0x00];
        let result = FileHeader::decode(&mut Cursor::new(&data));
        assert!(result.is_err());
    }

    #[test]
    fn file_header_rejects_invalid_bits() {
        let mut data = VCDIFF_MAGIC.to_vec();
        data.push(0xFF); // all bits set
        let result = FileHeader::decode(&mut Cursor::new(&data));
        assert!(result.is_err());
    }

    #[test]
    fn window_header_roundtrip_no_source() {
        let wh = WindowHeader {
            win_ind: VCD_ADLER32,
            copy_window_len: 0,
            copy_window_offset: 0,
            enc_len: 0, // will be recomputed
            target_window_len: 100,
            del_ind: 0,
            data_len: 30,
            inst_len: 20,
            addr_len: 10,
            adler32: Some(0xDEADBEEF),
        };
        let enc_len = wh.compute_enc_len();
        let wh = WindowHeader { enc_len, ..wh };

        let mut buf = Vec::new();
        wh.encode(&mut buf).unwrap();

        let decoded = WindowHeader::decode(&mut Cursor::new(&buf))
            .unwrap()
            .unwrap();
        assert_eq!(decoded.win_ind, VCD_ADLER32);
        assert_eq!(decoded.target_window_len, 100);
        assert_eq!(decoded.data_len, 30);
        assert_eq!(decoded.inst_len, 20);
        assert_eq!(decoded.addr_len, 10);
        assert_eq!(decoded.adler32, Some(0xDEADBEEF));
    }

    #[test]
    fn window_header_roundtrip_with_source() {
        let wh = WindowHeader {
            win_ind: VCD_SOURCE | VCD_ADLER32,
            copy_window_len: 65536,
            copy_window_offset: 1024,
            enc_len: 0,
            target_window_len: 4096,
            del_ind: 0,
            data_len: 1000,
            inst_len: 500,
            addr_len: 200,
            adler32: Some(0x12345678),
        };
        let enc_len = wh.compute_enc_len();
        let wh = WindowHeader { enc_len, ..wh };

        let mut buf = Vec::new();
        wh.encode(&mut buf).unwrap();

        let decoded = WindowHeader::decode(&mut Cursor::new(&buf))
            .unwrap()
            .unwrap();
        assert_eq!(decoded.copy_window_len, 65536);
        assert_eq!(decoded.copy_window_offset, 1024);
        assert_eq!(decoded.target_window_len, 4096);
    }

    #[test]
    fn window_header_eof_returns_none() {
        let data: &[u8] = &[];
        let result = WindowHeader::decode(&mut Cursor::new(data)).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn window_header_rejects_both_source_and_target() {
        let data = [VCD_SOURCE | VCD_TARGET]; // both set
        let result = WindowHeader::decode(&mut Cursor::new(&data));
        assert!(result.is_err());
    }

    #[test]
    fn adler32_is_big_endian() {
        let wh = WindowHeader {
            win_ind: VCD_ADLER32,
            target_window_len: 1,
            data_len: 0,
            inst_len: 0,
            addr_len: 0,
            adler32: Some(0xAABBCCDD),
            ..Default::default()
        };
        let enc_len = wh.compute_enc_len();
        let wh = WindowHeader { enc_len, ..wh };

        let mut buf = Vec::new();
        wh.encode(&mut buf).unwrap();

        // The last 4 bytes should be the checksum in big-endian.
        let tail = &buf[buf.len() - 4..];
        assert_eq!(tail, &[0xAA, 0xBB, 0xCC, 0xDD]);
    }
}
