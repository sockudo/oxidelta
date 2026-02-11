// VCDIFF default code table (RFC 3284, Section 5.6).
//
// Byte-for-byte compatible with xdelta3's `xd3_build_code_table` using the
// `__rfc3284_code_table_desc` descriptor.  The generated table has exactly
// 256 entries.

/// Instruction types matching xdelta3's `xd3_rtype` constants.
pub const XD3_NOOP: u8 = 0;
pub const XD3_ADD: u8 = 1;
pub const XD3_RUN: u8 = 2;
/// COPY modes are represented as XD3_CPY + mode (0..8 for default table).
pub const XD3_CPY: u8 = 3;

/// Minimum match length for COPY instructions (RFC 3284).
pub const MIN_MATCH: u8 = 4;

/// A single entry in the 256-element VCDIFF code table.
///
/// Each opcode can encode one or two instructions.  When `type2 == XD3_NOOP`,
/// the opcode encodes a single instruction.  When `size1 == 0` (or `size2 == 0`),
/// the actual size is read as a varint from the instruction stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeTableEntry {
    pub type1: u8,
    pub size1: u8,
    pub type2: u8,
    pub size2: u8,
}

/// The complete 256-entry VCDIFF code table.
pub type CodeTable = [CodeTableEntry; 256];

/// Build the default RFC 3284 code table.
///
/// This is an exact Rust translation of xdelta3's `xd3_build_code_table`
/// with the `__rfc3284_code_table_desc` descriptor.
pub fn build_default_code_table() -> CodeTable {
    let mut tbl = [CodeTableEntry::default(); 256];
    let mut idx: usize = 0;

    // Descriptor constants (from __rfc3284_code_table_desc).
    const ADD_SIZES: u8 = 17;
    const NEAR_MODES: usize = 4;
    const SAME_MODES: usize = 3;
    const CPY_SIZES: u8 = 15;
    const ADDCOPY_ADD_MAX: u8 = 4;
    const ADDCOPY_NEAR_CPY_MAX: u8 = 6;
    const ADDCOPY_SAME_CPY_MAX: u8 = 4;
    const COPYADD_ADD_MAX: u8 = 1;
    const COPYADD_NEAR_CPY_MAX: u8 = 4;
    const COPYADD_SAME_CPY_MAX: u8 = 4;
    const CPY_MODES: usize = 2 + NEAR_MODES + SAME_MODES; // 9

    // --- Index 0: RUN size=0 ---
    tbl[idx] = CodeTableEntry {
        type1: XD3_RUN,
        size1: 0,
        type2: XD3_NOOP,
        size2: 0,
    };
    idx += 1;

    // --- Index 1: ADD size=0 ---
    tbl[idx] = CodeTableEntry {
        type1: XD3_ADD,
        size1: 0,
        type2: XD3_NOOP,
        size2: 0,
    };
    idx += 1;

    // --- Indices 2..18: ADD size=1..17 ---
    for size1 in 1..=ADD_SIZES {
        tbl[idx] = CodeTableEntry {
            type1: XD3_ADD,
            size1,
            type2: XD3_NOOP,
            size2: 0,
        };
        idx += 1;
    }

    // --- COPY instructions: for each mode, size=0 then sizes 4..18 ---
    for mode in 0..CPY_MODES as u8 {
        // size=0 (size follows as varint)
        tbl[idx] = CodeTableEntry {
            type1: XD3_CPY + mode,
            size1: 0,
            type2: XD3_NOOP,
            size2: 0,
        };
        idx += 1;

        // sizes MIN_MATCH..MIN_MATCH+CPY_SIZES-1
        for size1 in MIN_MATCH..MIN_MATCH + CPY_SIZES {
            tbl[idx] = CodeTableEntry {
                type1: XD3_CPY + mode,
                size1,
                type2: XD3_NOOP,
                size2: 0,
            };
            idx += 1;
        }
    }

    // --- ADD+COPY double instructions ---
    for mode in 0..CPY_MODES as u8 {
        let near_limit = 2 + NEAR_MODES as u8;
        let cpy_max = if mode < near_limit {
            ADDCOPY_NEAR_CPY_MAX
        } else {
            ADDCOPY_SAME_CPY_MAX
        };

        for add_size in 1..=ADDCOPY_ADD_MAX {
            for cpy_size in MIN_MATCH..=cpy_max {
                tbl[idx] = CodeTableEntry {
                    type1: XD3_ADD,
                    size1: add_size,
                    type2: XD3_CPY + mode,
                    size2: cpy_size,
                };
                idx += 1;
            }
        }
    }

    // --- COPY+ADD double instructions ---
    for mode in 0..CPY_MODES as u8 {
        let near_limit = 2 + NEAR_MODES as u8;
        let cpy_max = if mode < near_limit {
            COPYADD_NEAR_CPY_MAX
        } else {
            COPYADD_SAME_CPY_MAX
        };

        for cpy_size in MIN_MATCH..=cpy_max {
            for add_size in 1..=COPYADD_ADD_MAX {
                tbl[idx] = CodeTableEntry {
                    type1: XD3_CPY + mode,
                    size1: cpy_size,
                    type2: XD3_ADD,
                    size2: add_size,
                };
                idx += 1;
            }
        }
    }

    debug_assert_eq!(idx, 256, "code table must have exactly 256 entries");
    tbl
}

/// Return a reference to the lazily-initialized default code table.
pub fn default_code_table() -> &'static CodeTable {
    use std::sync::LazyLock;
    static TABLE: LazyLock<CodeTable> = LazyLock::new(build_default_code_table);
    &TABLE
}

// ---------------------------------------------------------------------------
// Instruction chooser (encoder side)
//
// Matches xdelta3's `xd3_choose_instruction`.
// ---------------------------------------------------------------------------

/// Result of `choose_instruction`: the primary opcode and an optional
/// double-instruction opcode that merges this instruction with the previous one.
#[derive(Debug, Clone, Copy)]
pub struct ChosenInstruction {
    /// Single-instruction opcode for this instruction alone.
    pub code1: u8,
    /// If `Some`, a double opcode that encodes the *previous* instruction
    /// together with this one.  The previous instruction's `code2` should
    /// be set to this value.
    pub code2: Option<u8>,
}

/// Instruction descriptor passed to `choose_instruction`.
#[derive(Debug, Clone, Copy)]
pub struct InstructionInfo {
    /// XD3_ADD, XD3_RUN, or XD3_CPY + mode.
    pub itype: u8,
    /// Instruction size.
    pub size: u32,
}

/// Choose opcode(s) for an instruction, potentially forming a double
/// instruction with `prev` (the previously queued instruction).
///
/// Exact match of xdelta3's `xd3_choose_instruction`.
pub fn choose_instruction(
    prev: Option<&InstructionInfo>,
    inst: &InstructionInfo,
) -> ChosenInstruction {
    match inst.itype {
        XD3_RUN => ChosenInstruction {
            code1: 0,
            code2: None,
        },

        XD3_ADD => {
            let mut code1 = 1u8;
            let mut code2 = None;

            if inst.size <= 17 {
                code1 += inst.size as u8; // codes 2..18

                if inst.size == 1
                    && let Some(prev) = prev
                    && prev.size == 4
                    && prev.itype >= XD3_CPY
                {
                    // COPY(4,mode)+ADD(1) double
                    code2 = Some(247 + (prev.itype - XD3_CPY));
                }
            }

            ChosenInstruction { code1, code2 }
        }

        _ => {
            // XD3_CPY + mode
            let mode = inst.itype - XD3_CPY;
            let mut code1 = 19 + 16 * mode; // base for this mode, size=0
            let mut code2 = None;

            if inst.size >= 4 && inst.size <= 18 {
                code1 += (inst.size as u8) - 3; // size 4 -> +1, ... size 18 -> +15

                if let Some(prev) = prev
                    && prev.itype == XD3_ADD
                    && prev.size <= 4
                {
                    if inst.size <= 6 && mode <= 5 {
                        // ADD(1..4)+COPY(4..6) for modes 0..5
                        code2 = Some(
                            163 + (mode * 12)
                                + (3 * ((prev.size as u8) - 1))
                                + ((inst.size as u8) - 4),
                        );
                    } else if inst.size == 4 && mode >= 6 {
                        // ADD(1..4)+COPY(4) for modes 6..8
                        code2 = Some(235 + ((mode - 6) * 4) + ((prev.size as u8) - 1));
                    }
                }
            }

            ChosenInstruction { code1, code2 }
        }
    }
}

// ---------------------------------------------------------------------------
// High-level instruction type for public API
// ---------------------------------------------------------------------------

/// Decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instruction {
    /// Emit `len` literal bytes from the data section.
    Add { len: u32 },
    /// Copy `len` bytes starting at `addr` (in the combined source+target address space).
    Copy { len: u32, addr: u64, mode: u8 },
    /// Repeat a single byte (read from data section) `len` times.
    Run { len: u32 },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_256_entries() {
        let table = build_default_code_table();
        assert_eq!(table.len(), 256);
    }

    #[test]
    fn index_0_is_run() {
        let t = default_code_table();
        assert_eq!(t[0].type1, XD3_RUN);
        assert_eq!(t[0].size1, 0);
        assert_eq!(t[0].type2, XD3_NOOP);
    }

    #[test]
    fn index_1_is_add_size0() {
        let t = default_code_table();
        assert_eq!(t[1].type1, XD3_ADD);
        assert_eq!(t[1].size1, 0);
        assert_eq!(t[1].type2, XD3_NOOP);
    }

    #[test]
    fn indices_2_to_18_are_add() {
        let t = default_code_table();
        for (i, size) in (2..=18).zip(1..=17u8) {
            assert_eq!(t[i].type1, XD3_ADD, "index {i}");
            assert_eq!(t[i].size1, size, "index {i}");
            assert_eq!(t[i].type2, XD3_NOOP, "index {i}");
        }
    }

    #[test]
    fn copy_mode_0_starts_at_19() {
        let t = default_code_table();
        // Index 19: CPY mode=0, size=0
        assert_eq!(t[19].type1, XD3_CPY);
        assert_eq!(t[19].size1, 0);
        // Index 20: CPY mode=0, size=4
        assert_eq!(t[20].type1, XD3_CPY);
        assert_eq!(t[20].size1, 4);
        // Index 34: CPY mode=0, size=18
        assert_eq!(t[34].type1, XD3_CPY);
        assert_eq!(t[34].size1, 18);
    }

    #[test]
    fn copy_mode_1_starts_at_35() {
        let t = default_code_table();
        assert_eq!(t[35].type1, XD3_CPY + 1);
        assert_eq!(t[35].size1, 0);
    }

    #[test]
    fn last_copy_mode_8() {
        let t = default_code_table();
        // Mode 8: starts at 19 + 8*16 = 147
        assert_eq!(t[147].type1, XD3_CPY + 8);
        assert_eq!(t[147].size1, 0);
        assert_eq!(t[162].type1, XD3_CPY + 8);
        assert_eq!(t[162].size1, 18);
    }

    #[test]
    fn add_copy_doubles_start_at_163() {
        let t = default_code_table();
        // Index 163: ADD(1)+CPY(4,mode=0)
        assert_eq!(t[163].type1, XD3_ADD);
        assert_eq!(t[163].size1, 1);
        assert_eq!(t[163].type2, XD3_CPY);
        assert_eq!(t[163].size2, 4);
    }

    #[test]
    fn copy_add_doubles_start_at_247() {
        let t = default_code_table();
        // Index 247: CPY(4,mode=0)+ADD(1)
        assert_eq!(t[247].type1, XD3_CPY);
        assert_eq!(t[247].size1, 4);
        assert_eq!(t[247].type2, XD3_ADD);
        assert_eq!(t[247].size2, 1);
    }

    #[test]
    fn index_255_is_last() {
        let t = default_code_table();
        // Index 255: CPY(4,mode=8)+ADD(1)
        assert_eq!(t[255].type1, XD3_CPY + 8);
        assert_eq!(t[255].size1, 4);
        assert_eq!(t[255].type2, XD3_ADD);
        assert_eq!(t[255].size2, 1);
    }

    #[test]
    fn all_doubles_have_nonzero_sizes() {
        let t = default_code_table();
        for (i, entry) in t.iter().enumerate() {
            if entry.type2 != XD3_NOOP {
                assert_ne!(entry.size1, 0, "double at {i} has size1=0");
                assert_ne!(entry.size2, 0, "double at {i} has size2=0");
            }
        }
    }

    #[test]
    fn choose_run() {
        let r = choose_instruction(
            None,
            &InstructionInfo {
                itype: XD3_RUN,
                size: 10,
            },
        );
        assert_eq!(r.code1, 0);
        assert!(r.code2.is_none());
    }

    #[test]
    fn choose_add_small() {
        for size in 1..=17u32 {
            let r = choose_instruction(
                None,
                &InstructionInfo {
                    itype: XD3_ADD,
                    size,
                },
            );
            assert_eq!(r.code1, 1 + size as u8);
        }
    }

    #[test]
    fn choose_add_large() {
        let r = choose_instruction(
            None,
            &InstructionInfo {
                itype: XD3_ADD,
                size: 18,
            },
        );
        assert_eq!(r.code1, 1); // size=0 entry, size emitted separately
    }

    #[test]
    fn choose_copy_mode0_size4() {
        let r = choose_instruction(
            None,
            &InstructionInfo {
                itype: XD3_CPY,
                size: 4,
            },
        );
        assert_eq!(r.code1, 20); // 19 + (4-3) = 20
    }

    #[test]
    fn choose_double_add_copy() {
        let prev = InstructionInfo {
            itype: XD3_ADD,
            size: 1,
        };
        let inst = InstructionInfo {
            itype: XD3_CPY,
            size: 4,
        };
        let r = choose_instruction(Some(&prev), &inst);
        assert_eq!(r.code2, Some(163)); // ADD(1)+CPY(4,mode=0)
    }

    #[test]
    fn choose_double_copy_add() {
        let prev = InstructionInfo {
            itype: XD3_CPY,
            size: 4,
        };
        let inst = InstructionInfo {
            itype: XD3_ADD,
            size: 1,
        };
        let r = choose_instruction(Some(&prev), &inst);
        assert_eq!(r.code2, Some(247)); // CPY(4,mode=0)+ADD(1)
    }

    #[test]
    fn choose_double_add_copy_mode6() {
        let prev = InstructionInfo {
            itype: XD3_ADD,
            size: 2,
        };
        let inst = InstructionInfo {
            itype: XD3_CPY + 6,
            size: 4,
        };
        let r = choose_instruction(Some(&prev), &inst);
        // 235 + (6-6)*4 + (2-1) = 236
        assert_eq!(r.code2, Some(236));
    }

    #[test]
    fn code_table_matches_descriptor_offsets() {
        // Verify the descriptor offsets from __rfc3284_code_table_desc.
        // addcopy_max_sizes[0] = {6, 163, 3} means:
        //   mode 0 add-copy doubles start at index 163, max cpy=6, mult=3
        let t = default_code_table();

        // Mode 0: starts at 163, 4 add sizes * 3 cpy sizes = 12 entries
        assert_eq!(t[163].type1, XD3_ADD);
        assert_eq!(t[163].type2, XD3_CPY);
        assert_eq!(t[174].type1, XD3_ADD);
        assert_eq!(t[174].type2, XD3_CPY);

        // Mode 1: starts at 175
        assert_eq!(t[175].type1, XD3_ADD);
        assert_eq!(t[175].type2, XD3_CPY + 1);

        // Modes 6,7,8 (SAME modes): 4 add * 1 cpy = 4 entries each
        // Mode 6 starts at 235
        assert_eq!(t[235].type1, XD3_ADD);
        assert_eq!(t[235].type2, XD3_CPY + 6);
        assert_eq!(t[235].size2, 4);

        // copyadd_max_sizes[0] = {4, 247, 1}
        assert_eq!(t[247].type1, XD3_CPY);
        assert_eq!(t[247].type2, XD3_ADD);
    }
}
