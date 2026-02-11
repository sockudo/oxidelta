// Instruction optimization pipeline.
//
// Optimizes the raw instruction stream from MatchEngine before VCDIFF encoding:
//   - Coalesce adjacent ADDs
//   - Coalesce adjacent COPYs with contiguous addresses
//   - Detect runs within ADD data
//   - Remove zero-length instructions

use crate::hash::config::MIN_RUN;
use crate::hash::rolling;
use crate::vcdiff::code_table::Instruction;

/// Optimize an instruction stream for better compression.
///
/// The input `instructions` must cover `target` exactly (sum of lengths == target.len()).
/// Returns an optimized instruction stream with the same coverage guarantee.
pub fn optimize(instructions: &[Instruction], target: &[u8]) -> Vec<Instruction> {
    if instructions.is_empty() {
        return Vec::new();
    }

    let mut coalesced: Vec<Instruction> = Vec::with_capacity(instructions.len());

    for inst in instructions {
        // Skip zero-length instructions.
        let len = inst_len(inst);
        if len == 0 {
            continue;
        }

        // Try to coalesce with the last instruction in result.
        let merged = if let Some(last) = coalesced.last() {
            try_coalesce(last, inst)
        } else {
            None
        };

        if let Some(merged) = merged {
            *coalesced.last_mut().unwrap() = merged;
        } else {
            coalesced.push(*inst);
        }
    }

    // Split ADDs that contain runs (using cached run-length implementation).
    let run_length = rolling::run_length_fn();
    let mut result = Vec::with_capacity(coalesced.len() + coalesced.len() / 2 + 8);
    split_add_runs(&coalesced, target, run_length, &mut result);

    debug_assert_eq!(
        result.iter().map(|i| inst_len(i) as usize).sum::<usize>(),
        target.len(),
        "instruction optimization broke length invariant"
    );

    result
}

/// Try to merge two adjacent instructions into one.
fn try_coalesce(a: &Instruction, b: &Instruction) -> Option<Instruction> {
    match (a, b) {
        // Adjacent ADDs → single ADD.
        (Instruction::Add { len: l1 }, Instruction::Add { len: l2 }) => {
            Some(Instruction::Add { len: l1 + l2 })
        }

        // Adjacent COPYs with contiguous source addresses and same mode.
        (
            Instruction::Copy {
                len: l1,
                addr: a1,
                mode: m1,
            },
            Instruction::Copy {
                len: l2,
                addr: a2,
                mode: m2,
            },
        ) if *m2 == *m1 && *a2 == *a1 + *l1 as u64 => Some(Instruction::Copy {
            len: l1 + l2,
            addr: *a1,
            mode: *m1,
        }),

        // Adjacent RUNs of the same byte → single RUN.
        // (We don't know the byte here, so we can't verify — but adjacent RUNs
        // in the match engine output always have the same byte since they come
        // from contiguous target positions with identical bytes.)
        (Instruction::Run { len: l1 }, Instruction::Run { len: l2 }) => {
            Some(Instruction::Run { len: l1 + l2 })
        }

        _ => None,
    }
}

/// Scan ADD instructions for internal runs and split them out.
///
/// If an ADD covers target bytes [pos..pos+len] and there's a run of >= MIN_RUN
/// identical bytes inside, split into ADD(prefix) + RUN(run) + ADD(suffix).
fn split_add_runs(
    instructions: &[Instruction],
    target: &[u8],
    run_length: rolling::RunLengthFn,
    result: &mut Vec<Instruction>,
) {
    let mut target_pos = 0usize;

    for inst in instructions {
        match inst {
            Instruction::Add { len } => {
                let len = *len as usize;
                let data = &target[target_pos..target_pos + len];
                split_add_with_runs(data, run_length, result);
                target_pos += len;
            }
            Instruction::Copy { len, .. } | Instruction::Run { len } => {
                result.push(*inst);
                target_pos += *len as usize;
            }
        }
    }
}

/// Split a single ADD's data into ADD/RUN segments.
fn split_add_with_runs(data: &[u8], run_length: rolling::RunLengthFn, out: &mut Vec<Instruction>) {
    if data.is_empty() {
        return;
    }
    if data.len() < MIN_RUN {
        out.push(Instruction::Add {
            len: data.len() as u32,
        });
        return;
    }

    let mut i = 0;
    while i < data.len() {
        if data.len() - i < MIN_RUN {
            out.push(Instruction::Add {
                len: (data.len() - i) as u32,
            });
            break;
        }
        // Scan for a run starting at i (SIMD-accelerated).
        let byte = data[i];
        let run_len = run_length(&data[i..], byte, data.len() - i);

        if run_len >= MIN_RUN {
            // Emit the run.
            out.push(Instruction::Run {
                len: run_len as u32,
            });
            i += run_len;
        } else {
            // Scan forward to find the next run (or end of data).
            let add_start = i;
            i += run_len;
            while i < data.len() {
                let b = data[i];
                let rl = run_length(&data[i..], b, data.len() - i);
                if rl >= MIN_RUN {
                    break; // found a run, stop the ADD here
                }
                i += rl;
            }
            let add_len = i - add_start;
            out.push(Instruction::Add {
                len: add_len as u32,
            });
        }
    }
}

#[inline]
fn inst_len(inst: &Instruction) -> u32 {
    match inst {
        Instruction::Add { len } | Instruction::Copy { len, .. } | Instruction::Run { len } => *len,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn total_len(insts: &[Instruction]) -> usize {
        insts.iter().map(|i| inst_len(i) as usize).sum()
    }

    #[test]
    fn coalesce_adjacent_adds() {
        let target = b"Hello, world!";
        let insts = vec![
            Instruction::Add { len: 5 },
            Instruction::Add { len: 2 },
            Instruction::Add { len: 6 },
        ];
        let opt = optimize(&insts, target);
        assert_eq!(opt.len(), 1);
        assert!(matches!(opt[0], Instruction::Add { len: 13 }));
        assert_eq!(total_len(&opt), target.len());
    }

    #[test]
    fn coalesce_contiguous_copies() {
        let target = vec![0u8; 20];
        let insts = vec![
            Instruction::Copy {
                len: 10,
                addr: 0,
                mode: 0,
            },
            Instruction::Copy {
                len: 10,
                addr: 10,
                mode: 0,
            },
        ];
        let opt = optimize(&insts, &target);
        assert_eq!(opt.len(), 1);
        match opt[0] {
            Instruction::Copy { len, addr, .. } => {
                assert_eq!(len, 20);
                assert_eq!(addr, 0);
            }
            _ => panic!("expected COPY"),
        }
    }

    #[test]
    fn no_coalesce_noncontiguous_copies() {
        let target = vec![0u8; 20];
        let insts = vec![
            Instruction::Copy {
                len: 10,
                addr: 0,
                mode: 0,
            },
            Instruction::Copy {
                len: 10,
                addr: 100,
                mode: 0,
            },
        ];
        let opt = optimize(&insts, &target);
        assert_eq!(opt.len(), 2);
    }

    #[test]
    fn detect_run_in_add() {
        // Target: 3 bytes + 10 identical bytes + 3 bytes.
        let mut target = Vec::new();
        target.extend_from_slice(b"ABC");
        target.extend(std::iter::repeat_n(0xAA, 10));
        target.extend_from_slice(b"XYZ");

        let insts = vec![Instruction::Add {
            len: target.len() as u32,
        }];
        let opt = optimize(&insts, &target);

        // Should be ADD(3) + RUN(10) + ADD(3).
        assert_eq!(opt.len(), 3);
        assert!(matches!(opt[0], Instruction::Add { len: 3 }));
        assert!(matches!(opt[1], Instruction::Run { len: 10 }));
        assert!(matches!(opt[2], Instruction::Add { len: 3 }));
        assert_eq!(total_len(&opt), target.len());
    }

    #[test]
    fn no_run_below_threshold() {
        // Short runs (< MIN_RUN) should remain as ADD.
        let target = vec![0xAA; MIN_RUN - 1];
        let insts = vec![Instruction::Add {
            len: target.len() as u32,
        }];
        let opt = optimize(&insts, &target);
        assert_eq!(opt.len(), 1);
        assert!(matches!(opt[0], Instruction::Add { .. }));
    }

    #[test]
    fn remove_zero_length() {
        let target = b"Hello";
        let insts = vec![
            Instruction::Add { len: 0 },
            Instruction::Add { len: 5 },
            Instruction::Copy {
                len: 0,
                addr: 0,
                mode: 0,
            },
        ];
        let opt = optimize(&insts, target);
        assert_eq!(opt.len(), 1);
        assert!(matches!(opt[0], Instruction::Add { len: 5 }));
    }

    #[test]
    fn empty_instructions() {
        let opt = optimize(&[], b"");
        assert!(opt.is_empty());
    }

    #[test]
    fn coalesce_adjacent_runs() {
        let target = vec![0xBB; 20];
        let insts = vec![Instruction::Run { len: 10 }, Instruction::Run { len: 10 }];
        let opt = optimize(&insts, &target);
        assert_eq!(opt.len(), 1);
        assert!(matches!(opt[0], Instruction::Run { len: 20 }));
    }

    #[test]
    fn mixed_instructions_preserve_order() {
        let mut target = Vec::new();
        target.extend_from_slice(b"ABCD"); // ADD 4
        target.extend_from_slice(b"EFGH"); // COPY 4
        target.extend(std::iter::repeat_n(0xFF, 10)); // RUN 10
        target.extend_from_slice(b"IJ"); // ADD 2

        let insts = vec![
            Instruction::Add { len: 4 },
            Instruction::Copy {
                len: 4,
                addr: 100,
                mode: 0,
            },
            Instruction::Run { len: 10 },
            Instruction::Add { len: 2 },
        ];
        let opt = optimize(&insts, &target);
        // No coalescing possible (different types adjacent).
        assert_eq!(opt.len(), 4);
        assert_eq!(total_len(&opt), target.len());
    }
}
