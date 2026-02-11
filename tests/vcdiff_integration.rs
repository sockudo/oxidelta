// Comprehensive integration tests for VCDIFF encode/decode.
//
// These tests verify:
//   - End-to-end roundtrip for various file types and patterns
//   - Edge cases (empty, single-byte, large deltas)
//   - Format correctness (magic bytes, header structure, checksum)
//   - xdelta3 interoperability (if xdelta3 binary is available)
//   - Decoder robustness against malformed input

use oxidelta::vcdiff::{
    code_table::Instruction,
    decoder::{self, StreamDecoder},
    encoder::{SourceWindow, StreamEncoder, WindowEncoder},
    header::{FileHeader, VCD_ADLER32, VCDIFF_MAGIC, WindowHeader},
};
use std::io::Cursor;

// ===========================================================================
// Helpers
// ===========================================================================

/// Encode a target from instructions and decode it back, verifying roundtrip.
fn encode_decode_roundtrip(source: &[u8], target: &[u8], instructions: &[Instruction]) -> Vec<u8> {
    let src_win = if source.is_empty() {
        None
    } else {
        Some(SourceWindow {
            len: source.len() as u64,
            offset: 0,
        })
    };

    let mut we = WindowEncoder::new(src_win, true);
    let mut offset: usize = 0;

    for inst in instructions {
        match *inst {
            Instruction::Add { len } => {
                we.add(&target[offset..offset + len as usize]);
                offset += len as usize;
            }
            Instruction::Copy { len, addr, .. } => {
                we.copy_with_auto_mode(len, addr);
                offset += len as usize;
            }
            Instruction::Run { len } => {
                we.run(len, target[offset]);
                offset += len as usize;
            }
        }
    }

    let mut out = Vec::new();
    let mut enc = StreamEncoder::new(&mut out, true);
    enc.write_window(we, Some(target)).unwrap();
    let _ = enc.finish().unwrap();

    let decoded = decoder::decode_memory(&out, source).unwrap();
    assert_eq!(decoded, target, "roundtrip mismatch");
    out
}

/// Build a simple ADD-only delta for a target (no source).
fn add_only_delta(target: &[u8]) -> Vec<u8> {
    let instructions = vec![Instruction::Add {
        len: target.len() as u32,
    }];
    encode_decode_roundtrip(&[], target, &instructions)
}

// ===========================================================================
// Text file roundtrip tests
// ===========================================================================

#[test]
fn text_ascii_hello() {
    add_only_delta(b"Hello, world!");
}

#[test]
fn text_multiline() {
    let text = b"Line 1\nLine 2\nLine 3\nLine 4\n";
    add_only_delta(text);
}

#[test]
fn text_unicode_utf8() {
    let text = "Héllo, wörld! \u{1F600}\n日本語テスト\n".as_bytes();
    add_only_delta(text);
}

#[test]
fn text_large_lorem() {
    // ~4 KB of repeated text.
    let paragraph = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
        Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
        Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris. ";
    let mut text = Vec::new();
    for _ in 0..20 {
        text.extend_from_slice(paragraph);
    }
    add_only_delta(&text);
}

// ===========================================================================
// Binary data roundtrip tests
// ===========================================================================

#[test]
fn binary_all_bytes() {
    let data: Vec<u8> = (0..=255).collect();
    add_only_delta(&data);
}

#[test]
fn binary_zeros() {
    let data = vec![0u8; 1024];
    // Use RUN for efficiency.
    let instructions = vec![Instruction::Run { len: 1024 }];
    encode_decode_roundtrip(&[], &data, &instructions);
}

#[test]
fn binary_random_like() {
    // Deterministic pseudo-random data (LCG).
    let mut data = Vec::with_capacity(4096);
    let mut state: u32 = 0xDEADBEEF;
    for _ in 0..4096 {
        state = state.wrapping_mul(1103515245).wrapping_add(12345);
        data.push((state >> 16) as u8);
    }
    add_only_delta(&data);
}

// ===========================================================================
// Source-copy (delta) tests
// ===========================================================================

#[test]
fn delta_identical_files() {
    let source = b"The quick brown fox jumps over the lazy dog";
    let target = source;
    let instructions = vec![Instruction::Copy {
        len: source.len() as u32,
        addr: 0,
        mode: 0,
    }];
    encode_decode_roundtrip(source, target, &instructions);
}

#[test]
fn delta_small_edit() {
    let source = b"Hello, world!";
    let target = b"Hello, Rust!!";
    // "Hello, " from source, "Rust!!" as ADD
    let instructions = vec![
        Instruction::Copy {
            len: 7,
            addr: 0,
            mode: 0,
        }, // "Hello, "
        Instruction::Add { len: 6 }, // "Rust!!"
    ];
    encode_decode_roundtrip(source, target, &instructions);
}

#[test]
fn delta_prepend_and_append() {
    let source = b"middle";
    let target = b"[prefix]middle[suffix]";
    let instructions = vec![
        Instruction::Add { len: 8 }, // "[prefix]"
        Instruction::Copy {
            len: 6,
            addr: 0,
            mode: 0,
        }, // "middle"
        Instruction::Add { len: 8 }, // "[suffix]"
    ];
    encode_decode_roundtrip(source, target, &instructions);
}

#[test]
fn delta_multiple_copies() {
    let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    // Target: "ABCD" + "MNOP" + "WXYZ"
    let target = b"ABCDMNOPWXYZ";
    let instructions = vec![
        Instruction::Copy {
            len: 4,
            addr: 0,
            mode: 0,
        }, // "ABCD"
        Instruction::Copy {
            len: 4,
            addr: 12,
            mode: 0,
        }, // "MNOP"
        Instruction::Copy {
            len: 4,
            addr: 22,
            mode: 0,
        }, // "WXYZ"
    ];
    encode_decode_roundtrip(source, target, &instructions);
}

// ===========================================================================
// Target self-copy tests
// ===========================================================================

#[test]
fn self_copy_repeat() {
    // "ABCDABCD" using ADD + self-copy.
    let target = b"ABCDABCD";
    let instructions = vec![
        Instruction::Add { len: 4 },
        Instruction::Copy {
            len: 4,
            addr: 0,
            mode: 0,
        },
    ];
    encode_decode_roundtrip(&[], target, &instructions);
}

#[test]
fn self_copy_overlapping_rle() {
    // "AAAAAAAAAA" using ADD("A") + overlapping self-copy of 9.
    let target = b"AAAAAAAAAA";
    let instructions = vec![
        Instruction::Add { len: 1 },
        Instruction::Copy {
            len: 9,
            addr: 0,
            mode: 0,
        },
    ];
    encode_decode_roundtrip(&[], target, &instructions);
}

#[test]
fn self_copy_pattern_expansion() {
    // ADD "ABCA", then overlapping self-copy from offset 0 for 8 bytes.
    // Byte-by-byte copy reads: A B C A A B C A -> "ABCAABCA"
    // Full target: "ABCA" + "ABCAABCA" = "ABCAABCAABCA"
    let target = b"ABCAABCAABCA";
    let instructions = vec![
        Instruction::Add { len: 4 }, // "ABCA"
        Instruction::Copy {
            len: 8,
            addr: 0,
            mode: 0,
        }, // overlapping self-copy
    ];
    encode_decode_roundtrip(&[], target, &instructions);
}

// ===========================================================================
// RUN instruction tests
// ===========================================================================

#[test]
fn run_single_byte() {
    let target = vec![0xFF; 256];
    let instructions = vec![Instruction::Run { len: 256 }];
    encode_decode_roundtrip(&[], &target, &instructions);
}

#[test]
fn run_mixed_with_add() {
    // "AAAA" + "hello" + "BBBB"
    let target = b"AAAAhelloBBBB";
    let instructions = vec![
        Instruction::Run { len: 4 },
        Instruction::Add { len: 5 },
        Instruction::Run { len: 4 },
    ];
    encode_decode_roundtrip(&[], target, &instructions);
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
fn empty_target() {
    let target: &[u8] = b"";
    let we = WindowEncoder::new(None, false);
    // No instructions -- empty window.
    let mut out = Vec::new();
    let mut enc = StreamEncoder::new(&mut out, false);
    enc.write_window(we, Some(target)).unwrap();
    let _ = enc.finish().unwrap();

    let decoded = decoder::decode_memory(&out, &[]).unwrap();
    assert_eq!(decoded, target);
}

#[test]
fn single_byte_target() {
    add_only_delta(b"X");
}

#[test]
fn large_add() {
    // ADD with size > 17 (requires varint size in inst section).
    let target = vec![0x42; 1000];
    let instructions = vec![Instruction::Add { len: 1000 }];
    encode_decode_roundtrip(&[], &target, &instructions);
}

#[test]
fn large_copy() {
    // COPY with size > 18 (requires varint size in inst section).
    let source = vec![0x55; 10_000];
    let target = source.clone();
    let instructions = vec![Instruction::Copy {
        len: 10_000,
        addr: 0,
        mode: 0,
    }];
    encode_decode_roundtrip(&source, &target, &instructions);
}

#[test]
fn large_run() {
    let target = vec![0xCC; 100_000];
    let instructions = vec![Instruction::Run { len: 100_000 }];
    encode_decode_roundtrip(&[], &target, &instructions);
}

#[test]
fn many_small_instructions() {
    // 1000 single-byte ADDs.
    let target: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
    let instructions: Vec<_> = (0..1000).map(|_| Instruction::Add { len: 1 }).collect();
    encode_decode_roundtrip(&[], &target, &instructions);
}

// ===========================================================================
// Format correctness tests
// ===========================================================================

#[test]
fn magic_bytes_present() {
    let delta = add_only_delta(b"test");
    assert_eq!(&delta[..4], &VCDIFF_MAGIC);
}

#[test]
fn header_indicator_byte() {
    let delta = add_only_delta(b"test");
    // No secondary, no custom table, no app header → hdr_ind = 0.
    assert_eq!(delta[4], 0);
}

#[test]
fn window_has_checksum() {
    let delta = add_only_delta(b"test data for checksum verification");
    // Parse to verify checksum is present.
    let mut cursor = Cursor::new(&delta);
    let _fh = FileHeader::decode(&mut cursor).unwrap();
    let wh = WindowHeader::decode(&mut cursor).unwrap().unwrap();
    assert!(wh.has_checksum());
    assert!(wh.adler32.is_some());
}

#[test]
fn app_header_roundtrip() {
    let mut out = Vec::new();
    let mut enc = StreamEncoder::new(&mut out, false);
    enc.set_app_header(b"my app header".to_vec());
    let we = WindowEncoder::new(None, false);
    enc.write_window(we, Some(b"")).unwrap();
    let _ = enc.finish().unwrap();

    let mut dec = StreamDecoder::new(Cursor::new(&out), false);
    let fh = dec.read_header().unwrap();
    assert_eq!(fh.app_header.as_deref(), Some(b"my app header".as_slice()));
}

#[test]
fn enc_len_redundancy_check() {
    // Build a delta, tamper with enc_len, verify decoder rejects it.
    let delta = add_only_delta(b"hello");
    let mut tampered = delta.clone();
    // Find the window header (starts after 5-byte file header for hdr_ind=0).
    // Byte 5 is win_ind, then enc_len follows.
    // win_ind = VCD_ADLER32 = 0x04 (1 byte), then enc_len as varint.
    // We'll increment the enc_len byte to cause a mismatch.
    let win_ind_pos = 5; // after 4 magic + 1 hdr_ind
    assert_eq!(tampered[win_ind_pos] & 0x04, 0x04); // VCD_ADLER32
    // enc_len is at position 6 (varint, likely 1 byte for small windows).
    tampered[win_ind_pos + 1] = tampered[win_ind_pos + 1].wrapping_add(1);
    let result = decoder::decode_memory(&tampered, &[]);
    assert!(result.is_err());
}

// ===========================================================================
// Decoder robustness / malformed input tests
// ===========================================================================

#[test]
fn reject_bad_magic() {
    let data = [0x00, 0x00, 0x00, 0x00, 0x00];
    let result = decoder::decode_memory(&data, &[]);
    assert!(result.is_err());
}

#[test]
fn reject_truncated_header() {
    let data = [0xD6, 0xC3]; // truncated magic
    let result = decoder::decode_memory(&data, &[]);
    assert!(result.is_err());
}

#[test]
fn reject_invalid_header_bits() {
    let mut data = VCDIFF_MAGIC.to_vec();
    data.push(0xFF); // all indicator bits set (invalid)
    let result = decoder::decode_memory(&data, &[]);
    assert!(result.is_err());
}

#[test]
fn reject_truncated_window() {
    // Valid file header, then truncated window.
    let mut data = VCDIFF_MAGIC.to_vec();
    data.push(0x00); // hdr_ind
    data.push(VCD_ADLER32); // win_ind
    // Missing the rest of the window header.
    let result = decoder::decode_memory(&data, &[]);
    assert!(result.is_err());
}

#[test]
fn reject_window_too_large() {
    // Craft a window header claiming a huge target size.
    let mut data = VCDIFF_MAGIC.to_vec();
    data.push(0x00); // hdr_ind
    data.push(0x00); // win_ind (no source, no checksum)
    // enc_len = huge (but we'll put a valid structure)
    // This should fail the HARD_MAX_WINSIZE check.
    // Encode target_window_len > 16 MiB.
    let huge_size = 0x02_000_000u64; // 32 MiB
    // enc_len (we need to compute it, but just use a big value)
    let mut enc_len_buf = [0u8; 10];
    let enc_len_len = oxidelta::vcdiff::varint::encode_u64(100, &mut enc_len_buf);
    data.extend_from_slice(&enc_len_buf[10 - enc_len_len..]);
    // target_window_len
    let mut tgt_buf = [0u8; 10];
    let tgt_len = oxidelta::vcdiff::varint::encode_u64(huge_size, &mut tgt_buf);
    data.extend_from_slice(&tgt_buf[10 - tgt_len..]);

    let result = decoder::decode_memory(&data, &[]);
    assert!(result.is_err());
}

// ===========================================================================
// Double instruction packing tests
// ===========================================================================

#[test]
fn double_add_copy_packing() {
    // ADD(1) + COPY(4, mode=0) should produce a single double opcode (163).
    let source = b"ABCDEFGHIJ";
    let target = b"XABCD";
    let instructions = vec![
        Instruction::Add { len: 1 },
        Instruction::Copy {
            len: 4,
            addr: 0,
            mode: 0,
        },
    ];
    let delta = encode_decode_roundtrip(source, target, &instructions);

    // Parse and check inst section is compact.
    let mut cursor = Cursor::new(&delta);
    let _fh = FileHeader::decode(&mut cursor).unwrap();
    let wh = WindowHeader::decode(&mut cursor).unwrap().unwrap();
    // For a double instruction, inst_len should be 1 (single opcode byte).
    assert_eq!(
        wh.inst_len, 1,
        "expected double-packed instruction (1 byte)"
    );
}

#[test]
fn double_copy_add_packing() {
    // COPY(4, mode=0) + ADD(1) should produce opcode 247.
    let source = b"ABCDEFGHIJ";
    let target = b"ABCDX";
    let instructions = vec![
        Instruction::Copy {
            len: 4,
            addr: 0,
            mode: 0,
        },
        Instruction::Add { len: 1 },
    ];
    let delta = encode_decode_roundtrip(source, target, &instructions);

    let mut cursor = Cursor::new(&delta);
    let _fh = FileHeader::decode(&mut cursor).unwrap();
    let wh = WindowHeader::decode(&mut cursor).unwrap().unwrap();
    assert_eq!(
        wh.inst_len, 1,
        "expected double-packed instruction (1 byte)"
    );
}

// ===========================================================================
// xdelta3 binary interoperability tests
//
// These tests are only run if `xdelta3` is available on PATH.
// They verify that our encoder produces output that xdelta3 can decode,
// and that we can decode xdelta3-produced output.
// ===========================================================================

fn xdelta3_available() -> bool {
    std::process::Command::new("xdelta3")
        .arg("-V")
        .output()
        .is_ok()
}

#[test]
fn xdelta3_can_decode_our_output() {
    if !xdelta3_available() {
        eprintln!("SKIP: xdelta3 not found on PATH");
        return;
    }

    let source = b"The quick brown fox jumps over the lazy dog.";
    let target = b"The quick brown cat jumps over the lazy dog!";

    // Encode with our library.
    let instructions = vec![
        Instruction::Copy {
            len: 16,
            addr: 0,
            mode: 0,
        }, // "The quick brown "
        Instruction::Add { len: 4 }, // "cat "
        Instruction::Copy {
            len: 20,
            addr: 20,
            mode: 0,
        }, // "jumps over the lazy "
        Instruction::Add { len: 4 }, // "dog!"
    ];

    let src_win = SourceWindow {
        len: source.len() as u64,
        offset: 0,
    };
    let mut we = WindowEncoder::new(Some(src_win), true);

    let mut offset = 0usize;
    for inst in &instructions {
        match *inst {
            Instruction::Add { len } => {
                we.add(&target[offset..offset + len as usize]);
                offset += len as usize;
            }
            Instruction::Copy { len, addr, .. } => {
                we.copy_with_auto_mode(len, addr);
                offset += len as usize;
            }
            Instruction::Run { len } => {
                we.run(len, target[offset]);
                offset += len as usize;
            }
        }
    }

    let mut delta = Vec::new();
    let mut enc = StreamEncoder::new(&mut delta, true);
    enc.write_window(we, Some(target)).unwrap();
    let _ = enc.finish().unwrap();

    // Write source, delta to temp files.
    let dir = std::env::temp_dir().join("xdelta_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("source.bin");
    let delta_path = dir.join("delta.vcdiff");
    let out_path = dir.join("output.bin");
    std::fs::write(&src_path, source).unwrap();
    std::fs::write(&delta_path, &delta).unwrap();

    // Decode with xdelta3.
    let status = std::process::Command::new("xdelta3")
        .args(["-d", "-s"])
        .arg(&src_path)
        .arg(&delta_path)
        .arg(&out_path)
        .arg("-f")
        .status()
        .unwrap();

    if status.success() {
        let output = std::fs::read(&out_path).unwrap();
        assert_eq!(output, target, "xdelta3 decode mismatch");
    } else {
        panic!("xdelta3 decode failed with status: {status}");
    }

    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn we_can_decode_xdelta3_output() {
    if !xdelta3_available() {
        eprintln!("SKIP: xdelta3 not found on PATH");
        return;
    }

    let source = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let target = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ9876543210";

    let dir = std::env::temp_dir().join("xdelta_test2");
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("source.bin");
    let tgt_path = dir.join("target.bin");
    let delta_path = dir.join("delta.vcdiff");
    std::fs::write(&src_path, source).unwrap();
    std::fs::write(&tgt_path, target).unwrap();

    // Encode with xdelta3.
    let status = std::process::Command::new("xdelta3")
        .args(["-e", "-s"])
        .arg(&src_path)
        .arg(&tgt_path)
        .arg(&delta_path)
        .arg("-f")
        .arg("-n") // no checksum (for simplicity)
        .status()
        .unwrap();

    if !status.success() {
        panic!("xdelta3 encode failed");
    }

    // Decode with our library.
    let delta = std::fs::read(&delta_path).unwrap();
    let decoded = decoder::decode_memory(&delta, source).unwrap();
    assert_eq!(decoded, target, "our decoder mismatch on xdelta3 output");

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// Stress / property-style tests
// ===========================================================================

#[test]
fn roundtrip_varying_add_sizes() {
    // Test ADD instructions at every boundary around the code-table threshold (17).
    for size in [1, 2, 16, 17, 18, 100, 1000] {
        let target: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let instructions = vec![Instruction::Add { len: size as u32 }];
        encode_decode_roundtrip(&[], &target, &instructions);
    }
}

#[test]
fn roundtrip_varying_copy_sizes() {
    // Test COPY instructions at every boundary around code-table thresholds (4, 18).
    let source: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    for size in [4, 5, 17, 18, 19, 100, 5000] {
        let target = source[..size].to_vec();
        let instructions = vec![Instruction::Copy {
            len: size as u32,
            addr: 0,
            mode: 0,
        }];
        encode_decode_roundtrip(&source, &target, &instructions);
    }
}

#[test]
fn roundtrip_varying_run_sizes() {
    for size in [1, 7, 8, 100, 10000] {
        let target = vec![0xBB; size];
        let instructions = vec![Instruction::Run { len: size as u32 }];
        encode_decode_roundtrip(&[], &target, &instructions);
    }
}

#[test]
fn mixed_instruction_stress() {
    let source: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    // Build target: interleave copies, adds, runs.
    let mut target = Vec::new();
    let mut instructions = Vec::new();

    // COPY 100 bytes from source offset 0.
    target.extend_from_slice(&source[..100]);
    instructions.push(Instruction::Copy {
        len: 100,
        addr: 0,
        mode: 0,
    });

    // ADD 50 bytes.
    let add_data: Vec<u8> = (200..250).collect();
    target.extend_from_slice(&add_data);
    instructions.push(Instruction::Add { len: 50 });

    // RUN 30 bytes of 0xFF.
    target.extend(std::iter::repeat_n(0xFF, 30));
    instructions.push(Instruction::Run { len: 30 });

    // COPY 200 bytes from source offset 500.
    target.extend_from_slice(&source[500..700]);
    instructions.push(Instruction::Copy {
        len: 200,
        addr: 500,
        mode: 0,
    });

    // Self-copy: repeat the first 50 bytes of our target output.
    let self_copy_data: Vec<u8> = target[..50].to_vec();
    target.extend_from_slice(&self_copy_data);
    // Self-copy addr = source.len() + 0 = 1024.
    instructions.push(Instruction::Copy {
        len: 50,
        addr: source.len() as u64,
        mode: 0,
    });

    encode_decode_roundtrip(&source, &target, &instructions);
}
