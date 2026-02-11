// VCDIFF variable-length integer encoding (RFC 3284, Section 2).
//
// Base-128, big-endian: most-significant group first.
// Each byte has bit 7 set except the final byte.
// Identical to xdelta3's `xd3_emit_size` / `xd3_read_size`.

use std::io::{self, Read, Write};

/// Maximum encoded length for a 64-bit value (ceil(64/7) = 10).
const MAX_VARINT_LEN: usize = 10;

/// Overflow guard for 32-bit accumulator: if these bits are set before a
/// shift, the next `<< 7` would overflow.
const U32_OVERFLOW_MASK: u32 = 0xFE00_0000;

/// Overflow guard for 64-bit accumulator.
const U64_OVERFLOW_MASK: u64 = 0xFE00_0000_0000_0000;

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Encode a `u64` as a VCDIFF variable-length integer into `buf`.
/// Returns the number of bytes written (1..=10).
///
/// Matches xdelta3 `EMIT_INTEGER_TYPE`: fills a 10-byte scratch buffer from
/// the end, MSB set on all bytes, then clears MSB on the final (last) byte.
#[inline]
pub fn encode_u64(mut num: u64, buf: &mut [u8; MAX_VARINT_LEN]) -> usize {
    let mut i = MAX_VARINT_LEN;
    loop {
        i -= 1;
        buf[i] = (num as u8 & 0x7F) | 0x80;
        num >>= 7;
        if num == 0 {
            break;
        }
    }
    buf[MAX_VARINT_LEN - 1] &= 0x7F; // clear MSB on last byte
    MAX_VARINT_LEN - i
}

/// Encode a `u32` as a VCDIFF variable-length integer into `buf`.
/// Returns the number of bytes written (1..=5).
#[inline]
pub fn encode_u32(num: u32, buf: &mut [u8; MAX_VARINT_LEN]) -> usize {
    encode_u64(num as u64, buf)
}

/// Encode a `usize` and write to a `Write` sink.
pub fn write_usize<W: Write>(w: &mut W, num: usize) -> io::Result<()> {
    let mut buf = [0u8; MAX_VARINT_LEN];
    let len = encode_u64(num as u64, &mut buf);
    w.write_all(&buf[MAX_VARINT_LEN - len..])
}

/// Encode a `u64` and write to a `Write` sink.
pub fn write_u64<W: Write>(w: &mut W, num: u64) -> io::Result<()> {
    let mut buf = [0u8; MAX_VARINT_LEN];
    let len = encode_u64(num, &mut buf);
    w.write_all(&buf[MAX_VARINT_LEN - len..])
}

/// Encode a `u32` and write to a `Write` sink.
pub fn write_u32<W: Write>(w: &mut W, num: u32) -> io::Result<()> {
    write_u64(w, num as u64)
}

// ---------------------------------------------------------------------------
// Decoding from byte slices (non-streaming, matches `READ_INTEGER_TYPE`)
// ---------------------------------------------------------------------------

/// Decode a `u64` from a byte slice, advancing the cursor.
/// Returns `(value, bytes_consumed)` or an error.
///
/// Matches xdelta3 `READ_INTEGER_TYPE` with `UINT64_OFLOW_MASK`.
pub fn read_u64(data: &[u8]) -> Result<(u64, usize), VarIntError> {
    let mut val: u64 = 0;
    for (i, &byte) in data.iter().enumerate() {
        if val & U64_OVERFLOW_MASK != 0 {
            return Err(VarIntError::Overflow);
        }
        val = (val << 7) | u64::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Ok((val, i + 1));
        }
    }
    Err(VarIntError::Underflow)
}

/// Decode a `u32` from a byte slice, advancing the cursor.
pub fn read_u32(data: &[u8]) -> Result<(u32, usize), VarIntError> {
    let mut val: u32 = 0;
    for (i, &byte) in data.iter().enumerate() {
        if val & U32_OVERFLOW_MASK != 0 {
            return Err(VarIntError::Overflow);
        }
        val = (val << 7) | u32::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Ok((val, i + 1));
        }
    }
    Err(VarIntError::Underflow)
}

/// Decode a `usize` from a byte slice.
pub fn read_usize(data: &[u8]) -> Result<(usize, usize), VarIntError> {
    // Use u64 internally, then narrow with overflow check.
    let (val, len) = read_u64(data)?;
    let val = usize::try_from(val).map_err(|_| VarIntError::Overflow)?;
    Ok((val, len))
}

// ---------------------------------------------------------------------------
// Decoding from `Read` (streaming)
// ---------------------------------------------------------------------------

/// Read a `u64` varint from a streaming source.
pub fn stream_read_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut val: u64 = 0;
    let mut buf = [0u8; 1];
    loop {
        r.read_exact(&mut buf)?;
        let byte = buf[0];
        if val & U64_OVERFLOW_MASK != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "varint overflow",
            ));
        }
        val = (val << 7) | u64::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Ok(val);
        }
    }
}

/// Read a `u32` varint from a streaming source.
pub fn stream_read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut val: u32 = 0;
    let mut buf = [0u8; 1];
    loop {
        r.read_exact(&mut buf)?;
        let byte = buf[0];
        if val & U32_OVERFLOW_MASK != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "varint overflow",
            ));
        }
        val = (val << 7) | u32::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Ok(val);
        }
    }
}

/// Read a `usize` varint from a streaming source.
pub fn stream_read_usize<R: Read>(r: &mut R) -> io::Result<usize> {
    let val = stream_read_u64(r)?;
    usize::try_from(val).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "varint overflow"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the encoded byte-length of a `u64` value.
/// Matches xdelta3 `xd3_sizeof_uint64_t`.
#[inline]
pub fn sizeof_u64(num: u64) -> usize {
    let bits = 64 - num.leading_zeros();
    (bits.max(1).div_ceil(7) as usize).min(10)
}

/// Return the encoded byte-length of a `u32` value.
/// Matches xdelta3 `xd3_sizeof_uint32_t`.
#[inline]
pub fn sizeof_u32(num: u32) -> usize {
    let bits = 32 - num.leading_zeros();
    (bits.max(1).div_ceil(7) as usize).min(5)
}

/// Return the encoded byte-length of a `usize` value.
#[inline]
pub fn sizeof_usize(num: usize) -> usize {
    sizeof_u64(num as u64)
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarIntError {
    /// Not enough input bytes to complete the integer.
    Underflow,
    /// Value would overflow the target integer type.
    Overflow,
}

impl std::fmt::Display for VarIntError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VarIntError::Underflow => write!(f, "varint underflow (truncated input)"),
            VarIntError::Overflow => write!(f, "varint overflow"),
        }
    }
}

impl std::error::Error for VarIntError {}

impl From<VarIntError> for io::Error {
    fn from(e: VarIntError) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_u64() {
        let cases: &[u64] = &[
            0,
            1,
            127,
            128,
            255,
            256,
            16383,
            16384,
            u32::MAX as u64,
            u64::MAX,
        ];
        let mut buf = [0u8; MAX_VARINT_LEN];
        for &val in cases {
            let len = encode_u64(val, &mut buf);
            let (decoded, consumed) = read_u64(&buf[MAX_VARINT_LEN - len..]).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {val}");
            assert_eq!(consumed, len, "length mismatch for {val}");
            assert_eq!(sizeof_u64(val), len, "sizeof mismatch for {val}");
        }
    }

    #[test]
    fn roundtrip_u32() {
        let cases: &[u32] = &[0, 1, 127, 128, 16383, 16384, u32::MAX];
        let mut buf = [0u8; MAX_VARINT_LEN];
        for &val in cases {
            let len = encode_u32(val, &mut buf);
            let (decoded, consumed) = read_u32(&buf[MAX_VARINT_LEN - len..]).unwrap();
            assert_eq!(decoded, val);
            assert_eq!(consumed, len);
            assert_eq!(sizeof_u32(val), len);
        }
    }

    #[test]
    fn encoding_is_big_endian() {
        // 300 = 0b100101100 = two groups: (10) (0101100) = 0x82 0x2C
        let mut buf = [0u8; MAX_VARINT_LEN];
        let len = encode_u64(300, &mut buf);
        assert_eq!(len, 2);
        assert_eq!(&buf[MAX_VARINT_LEN - 2..], &[0x82, 0x2C]);
    }

    #[test]
    fn single_byte_values() {
        let mut buf = [0u8; MAX_VARINT_LEN];
        for val in 0..=127u64 {
            let len = encode_u64(val, &mut buf);
            assert_eq!(len, 1);
            assert_eq!(buf[MAX_VARINT_LEN - 1], val as u8);
        }
    }

    #[test]
    fn overflow_detection_u32() {
        // Encode u64::MAX and try to decode as u32 -- must fail.
        let mut buf = [0u8; MAX_VARINT_LEN];
        let len = encode_u64(u64::MAX, &mut buf);
        let result = read_u32(&buf[MAX_VARINT_LEN - len..]);
        assert_eq!(result, Err(VarIntError::Overflow));
    }

    #[test]
    fn underflow_detection() {
        // Truncated: all continuation bytes, no terminator.
        let data = [0x80, 0x80, 0x80];
        assert_eq!(read_u64(&data), Err(VarIntError::Underflow));
    }

    #[test]
    fn streaming_roundtrip() {
        let mut buf = [0u8; MAX_VARINT_LEN];
        let len = encode_u64(123456789, &mut buf);
        let bytes = &buf[MAX_VARINT_LEN - len..MAX_VARINT_LEN];
        let mut cursor = std::io::Cursor::new(bytes);
        let val = stream_read_u64(&mut cursor).unwrap();
        assert_eq!(val, 123456789);
    }

    #[test]
    fn write_read_roundtrip() {
        let mut out = Vec::new();
        write_u64(&mut out, 999999).unwrap();
        let (val, len) = read_u64(&out).unwrap();
        assert_eq!(val, 999999);
        assert_eq!(len, out.len());
    }
}
