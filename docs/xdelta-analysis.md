# Xdelta Repository Analysis

## Scope and Structure

This workspace has two layers:

- `src/main.rs`: a minimal Rust placeholder (`Hello, world!`).
- `xdelta/xdelta3/*`: the real C implementation (library + CLI) of Xdelta3/VCDIFF.

The analysis below focuses on `xdelta/xdelta3`, especially:

- `xdelta/xdelta3/xdelta3.c` (core engine and encoder)
- `xdelta/xdelta3/xdelta3-decode.h` (decoder state machine)
- `xdelta/xdelta3/xdelta3-main.h` (CLI, file I/O, external compression, recode/merge tools)
- `xdelta/xdelta3/xdelta3-hash.h` (rolling/hashed matching primitives)
- `xdelta/xdelta3/xdelta3-{djw,fgk,lzma}.h` (secondary compressors)
- `xdelta/xdelta3/xdelta3-blkcache.h` (source block cache and getblk callback glue)

---

## High-Level Architecture

Xdelta3 is organized around a re-entrant stream object (`xd3_stream`) that drives two state machines:

- Encode state machine (`ENC_INIT -> ENC_INPUT -> ENC_SEARCH -> ENC_INSTR -> ENC_FLUSH -> ENC_POSTOUT -> ENC_POSTWIN`) in `xdelta/xdelta3/xdelta3.c:3071`.
- Decode state machine (`DEC_VCHEAD -> ... -> DEC_FINISH`) in `xdelta/xdelta3/xdelta3-decode.h`.

CLI (`xdelta3-main.h`) wraps these APIs and adds:

- File I/O abstraction across POSIX/stdio/Win32.
- Optional external (subprocess-based) decompression/recompression.
- VCDIFF tools (`printhdr`, `printhdrs`, `printdelta`, `recode`, `merge`).
- App-header defaults and command-line parsing.

Core flow:

1. Configure stream (`xd3_config_stream`) with matching/compression flags.
2. Feed input windows (`xd3_avail_input` + `xd3_encode_input` / `xd3_decode_input`).
3. Handle return codes (`XD3_INPUT`, `XD3_OUTPUT`, `XD3_GETSRCBLK`, `XD3_GOTHEADER`, `XD3_WINSTART`, `XD3_WINFINISH`).
4. Consume output, then close/free stream.

---

## Core Algorithms

## 1) Rolling Hash and Match Discovery

### Small checksum path (target-to-target copies)

- Implemented in `xdelta/xdelta3/xdelta3-hash.h`.
- Uses a 4-byte read (`UNALIGNED_READ32`) multiplied by an LCG constant (`xd3_hash_multiplier32`).
- Update function shifts by one byte (`xd3_small_cksum_update`); note `look` is currently hardcoded effectively to 4 (comment in file).
- Hash bucket index is computed by `xd3_checksum_hash`.
- Candidate chains are traversed in `xd3_smatch` (`xdelta/xdelta3/xdelta3.c:4170`) using `small_prev` linked history (bounded by `small_chain` or `small_lchain`).

### Large checksum path (target-to-source copies)

- Polynomial/Rabin-Karp style rolling checksum in `xdelta/xdelta3/xdelta3-hash.h`.
- `xd3_large*_cksum`: weighted sum using precomputed powers.
- `xd3_large*_cksum_update`: rolling update using multiplier, outgoing byte, incoming byte.
- Source hash table is built incrementally by `xd3_srcwin_move_point` (`xdelta/xdelta3/xdelta3.c:4331`), which the source comments explicitly call one of the most expensive paths.

### Match expansion

- Source matches are validated/extended by `xd3_source_match_setup` + `xd3_source_extend_match` (`xdelta/xdelta3/xdelta3.c:3688`, `xdelta/xdelta3/xdelta3.c:3889`).
- Expansion supports backward and forward growth across source blocks, potentially re-entering via `XD3_GETSRCBLK`.
- Target self-matches are handled by `xd3_smatch`.
- Run-length detection exists via `xd3_comprun` + `xd3_emit_run`.

### Matching strategy profiles

Template-generated matchers are defined in `xdelta/xdelta3/xdelta3-cfgs.h`:

- `fastest`, `faster`, `fast`, `default`, `slow`, and `soft`.
- Tunables include `LLOOK`, `LSTEP`, `SCHAIN`, `SLCHAIN`, `MAXLAZY`, `LONGENOUGH`.
- CLI compression level maps to these profiles (see CLI section).

---

## 2) Instruction Selection and Encoding

### IOPT buffer and 1.5-pass optimization

- Candidate COPY/RUN instructions are queued in an instruction optimization buffer (`iopt_used`/`iopt_free`).
- Overlap resolution and pruning happen in `xd3_iopt_flush_instructions` (`xdelta/xdelta3/xdelta3.c:2313`).
- `xd3_iopt_erase` removes covered instructions when backward source extension finds better ranges.
- ADD instructions are synthesized around unmatched gaps (`xd3_iopt_add` / `xd3_iopt_add_finalize`).

### Code-table encoding and double-instruction packing

- Default RFC3284 table is generated from `__rfc3284_code_table_desc` in `xdelta/xdelta3/xdelta3.c:813`.
- Instruction emission chooses single or packed double opcodes (`xd3_emit_single`, `xd3_emit_double`) based on `xd3_choose_instruction`.
- Header/data/inst/addr are emitted into separate output chains, then concatenated for output.

### Address compression

- Address mode selection (`SELF`, `HERE`, `NEAR`, `SAME`) is done in `xd3_encode_address` (`xdelta/xdelta3/xdelta3.c:1276`).
- It greedily selects the encoding that minimizes integer byte length for the address delta.
- `xd3_update_cache` maintains the NEAR and SAME caches.

---

## 3) Compression Layers

### Primary (VCDIFF) encoding

- ADD/COPY/RUN instruction stream plus data/address side sections.
- Windowed encoding to keep memory bounded for normal encode/decode.

### Secondary compression (optional, per section)

- Implemented through `xd3_sec_type` and `xd3_encode_secondary`/`xd3_decode_secondary`.
- Supported algorithms (compile-time dependent):
  - DJW static/semi-adaptive Huffman (`xdelta3-djw.h`)
  - FGK adaptive Huffman (`xdelta3-fgk.h`)
  - LZMA (`xdelta3-lzma.h`)
- Can be enabled per section (DATA/INST/ADDR) using flags.

### External compression (CLI convenience layer)

- Detects compressed inputs by magic numbers and forks decompressor pipelines in `xdelta3-main.h`.
- Recompression can be applied on output in decode flows.
- Supported external types table (`extcomp_types`):
  - `bzip2`, `gzip`, `compress/uncompress`, `xz`

---

## VCDIFF Format Implementation Details

## Header and Window Fields

Decoder in `xdelta/xdelta3/xdelta3-decode.h` parses:

- File magic/version:
  - bytes: `0xD6 0xC3 0xC4 0x00`
- Header indicator bits:
  - `VCD_SECONDARY`, `VCD_CODETABLE`, `VCD_APPHEADER`
- Optional secondary ID
- Optional code-table block metadata
- Optional application header
- Per-window fields:
  - window indicator (`VCD_SOURCE`, `VCD_TARGET`, `VCD_ADLER32`)
  - copy window len/offset
  - encoding len
  - target window len
  - delta indicator (`VCD_DATACOMP`, `VCD_INSTCOMP`, `VCD_ADDRCOMP`)
  - data/inst/addr section lengths
  - optional Adler-32

## Variable-length integer coding

- Size/offset integers use base-128 varint style helpers in `xdelta3-internal.h` (`xd3_decode_size`, `xd3_emit_size`, etc.).
- Overflow checks are enforced during parse (`USIZE_T_OVERFLOW`, `XOFF_T_OVERFLOW`).

## Address modes and cache

- Mode decode/encode uses RFC NEAR/SAME cache semantics (`xd3_decode_address`, `xd3_encode_address`).
- Cache state is reset per window (`xd3_init_cache`) and updated per COPY.

## Section decode strategy

- Decoder can avoid copies when all section bytes are already contiguous in `avail_in` (`xd3_decode_section` zero-copy branch).
- Otherwise it allocates section staging buffers (`copied1`) and optionally second buffers after secondary decompression (`copied2`).

## Checksums

- Window checksum algorithm is Adler-32 only.
- Encode: optional per-window checksum emit.
- Decode: optional checksum verification; can be disabled with `XD3_ADLER32_NOVER`/`-n`.

## Explicitly unsupported or partial spec areas

- `VCD_CODETABLE` decode path returns unimplemented (`"VCD_CODETABLE support was removed"`).
- `VCD_TARGET` output reconstruction is marked unimplemented in decode setup.
- Default table encode path is fully implemented; arbitrary custom-table decode is not available in this branch.

---

## CLI Commands, Flags, and Behaviors

Parser is in `xdelta/xdelta3/xdelta3-main.h` and is custom (not `getopt`), with optional-argument support for `-A` and `-S`.

## Commands

- `encode` (default when encoder compiled)
- `decode`
- `config`
- `test` (only if compiled with regression tests)
- `printhdr`
- `printhdrs`
- `printdelta`
- `recode`
- `merge`

## Standard flags

| Flag | Behavior |
|---|---|
| `-0..-9` | Compression level (maps to matcher profile; `-0` also forces `XD3_NOCOMPRESS`). |
| `-c` | Force stdout output. |
| `-d` | Decode mode (equivalent to `decode` command). |
| `-e` | Encode mode (equivalent to `encode` command). |
| `-f` | Overwrite output; also used in external-compression force/garbage handling paths. |
| `-F` | Pass `-f` to external compressor/decompressor subprocesses (`EXTERNAL_COMPRESSION` builds only). |
| `-h` | Help/usage output. |
| `-q` | Quiet mode (suppresses verbose output). |
| `-v` | Increase verbosity (repeatable). |
| `-V` | Version output. |

## Memory/window flags

| Flag | Behavior |
|---|---|
| `-B bytes` | Source window size (`option_srcwinsz`), bounded by `[XD3_MINSRCWINSZ, XD3_MAXSRCWINSZ]`. |
| `-W bytes` | Input window size (`winsize`), bounded by `[XD3_ALLOCSIZE, XD3_HARDMAXWINSIZE]`. |
| `-P size` | Duplicate-window tracking size (`sprevsz`), affects small-match chain memory. |
| `-I size` | IOPT instruction buffer entries (`0` means unlimited growth mode). |

## Compression and metadata flags

| Flag | Behavior |
|---|---|
| `-s source` | Source file path for delta mode. |
| `-S [lzma\|djw\|fgk\|none]` | Secondary compression select; no argument disables secondary. If `-S` is omitted entirely, CLI defaults to LZMA when compiled, otherwise none. `djwN` (`N=0..9`) tunes section application aggressiveness. |
| `-N` | Disable small-string matching/target-side compression (`XD3_NOCOMPRESS`). |
| `-D` | Disable external input decompression (CLI wrapper feature). |
| `-R` | Disable external output recompression (CLI wrapper feature). |
| `-n` | Disable checksum generation/verification behavior. |
| `-C a,b,c,d,e,f,g` | Soft matcher tuning values (7 integers), undocumented/advanced. |
| `-A [apphead]` | With argument: set app header string; with no arg: disable app header. |
| `-J` | No output writes; decode/compute/check only. |
| `-m file` | Additional merge input (used by `merge` command). |

## Parsing and positional behavior details

- Positional arguments: up to two (`input`, `output`).
- If `input` missing: stdin.
- If `output` missing: derived later (appheader/default/stdout flow).
- `-c` overrides positional output filename.
- `XDELTA` environment variable is prepended as extra args (space-split, no shell-like quote parser).
- Optional arg forms accepted for `-A`/`-S`: `-Afoo`, `-A foo`, `-A=foo`, and no-arg variants.

## Command-specific execution behavior

- `encode`: uses `xd3_encode_input`.
- `decode`: uses `xd3_decode_input`.
- `printhdr`/`printhdrs`/`printdelta`: decode-only introspection with `XD3_JUST_HDR`/`XD3_SKIP_WINDOW`/`XD3_SKIP_EMIT`.
- `recode`: decodes each window and re-emits with updated appheader/secondary/checksum settings.
- `merge`: loads/combines whole-delta instruction streams in memory, then re-encodes.

## Notes on CLI docs vs code

- `xdelta3.1` includes `-T` (alternate code table), but parser in this tree does not implement `-T`.
- Option string still contains legacy letters (`E`, `L`, `O`, `M`) without switch handlers.

---

## Compression Level Mapping

CLI level to matcher config (`xdelta3-main.h`):

- `0`: `XD3_NOCOMPRESS` + `XD3_SMATCH_FASTEST`
- `1`: `XD3_SMATCH_FASTEST`
- `2`: `XD3_SMATCH_FASTER`
- `3..5`: `XD3_SMATCH_FAST`
- `6`: `XD3_SMATCH_DEFAULT`
- `7..9`: `XD3_SMATCH_SLOW`

Template profiles are defined in `xdelta/xdelta3/xdelta3-cfgs.h`.

---

## Performance-Critical Paths

These paths dominate CPU or end-to-end latency:

1. Source checksum indexing and source-window movement:
   - `xd3_srcwin_move_point` (`xdelta3.c:4331`)
2. Small-match candidate scan:
   - `xd3_smatch` (`xdelta3.c:4170`)
3. Source match extension across blocks:
   - `xd3_source_extend_match` (`xdelta3.c:3889`)
4. IOPT overlap compaction and emission:
   - `xd3_iopt_flush_instructions` (`xdelta3.c:2313`)
5. Address mode selection per COPY:
   - `xd3_encode_address` (`xdelta3.c:1276`)
6. Decode instruction execution:
   - `xd3_decode_output_halfinst` (`xdelta3-decode.h:401`) (commented as decoder hotspot)
7. Secondary compressors (especially DJW/FGK):
   - `xdelta3-djw.h`, `xdelta3-fgk.h`
8. External compression process setup:
   - multiple `fork/pipe/dup/exec` stages in `xdelta3-main.h`

---

## Data Structures and Relationships

## Core runtime graph

`xd3_stream` is the center object (`xdelta3.h:868`) and owns references/state for:

- Source context: `xd3_source *src`
- Hash configs/tables: `large_hash`, `small_hash`, `large_table`, `small_table`, `small_prev`
- Address cache: `xd3_addr_cache acache`
- Encoder instruction queues: `iopt_used`, `iopt_free`, `iopt_alloc`, `iout`
- Output section lists: `enc_heads[HDR/DATA/INST/ADDR]`, `enc_tails[]`, `enc_free`
- Decoder sections/buffers: `data_sect`, `inst_sect`, `addr_sect`, `dec_buffer`, `dec_lastwin`
- Secondary compression streams: `sec_stream_d`, `sec_stream_i`, `sec_stream_a`
- Whole-delta state for merge/recode tools: `whole_target`

## Supporting structures

- `xd3_rinst`: pending instruction record in IOPT queue.
- `xd3_output`: paged output block (`base`, `next`, `avail`, `next_page`).
- `xd3_desect`: decoded section view + allocated backing.
- `xd3_hash_cfg`: rolling-hash parameters and precomputed powers.
- `xd3_addr_cache`: NEAR and SAME caches.
- `xd3_whole_state`: fully materialized instruction/add buffers for merge/recode.

## CLI-side data structures

- `main_file`: OS-abstracted file handle + metadata/compressor context.
- `main_blklru` (blkcache): source block cache entries sharing one large allocation.
- `main_extcomp`: external compressor/decompressor command metadata.

---

## Memory Management Strategies

## Library memory strategy

- Allocation API is pluggable via callbacks (`config.alloc`, `config.freef`).
- Default library allocator is `malloc/free`.
- Many structures are lazy-allocated:
  - Hash tables and checksum tables allocated on first real matching use.
  - Decoder section buffers allocated only when section copy is required.
- Output uses fixed-size pages (`XD3_ALLOCSIZE`) linked as chains.
- IOPT can be fixed-size or unlimited:
  - `iopt_size > 0`: fixed queue, flushes when full.
  - `iopt_size == 0`: dynamically grows by chunk allocations.

## CLI memory strategy

- `main_bufalloc/main_buffree` use `VirtualAlloc/VirtualFree` on Win32, `malloc/free` elsewhere.
- Source window buffer:
  - One allocation of `option_srcwinsz`.
  - Partitioned into up to `MAX_LRU_SIZE` blocks (32) for LRU/FIFO behavior.
- `main_cleanup` and `xd3_free_stream` aggressively free all owned buffers and reset state.

## Bounded-memory guarantees and exceptions

- Normal encode/decode are windowed and bounded by configured window sizes.
- Decoder hard-limits window size (`XD3_HARDMAXWINSIZE`) to reject malicious inputs.
- `merge`/`recode` whole-state tools intentionally buffer complete instruction streams in memory (not constant-space).

---

## Feature Checklist

## Encoding/decoding modes

- [x] CLI encode (`-e` / `encode`)
- [x] CLI decode (`-d` / `decode`)
- [x] Library non-blocking API (`xd3_encode_input` / `xd3_decode_input`)
- [x] In-memory helpers (`xd3_encode_memory`, `xd3_decode_memory`)
- [x] VCDIFF inspection tools (`printhdr`, `printhdrs`, `printdelta`)
- [x] Re-encode existing VCDIFF (`recode`)
- [x] Merge multiple VCDIFFs (`merge`, `-m`)
- [ ] `VCD_TARGET` decode support (explicitly unimplemented)
- [ ] Custom `VCD_CODETABLE` decode support (removed/unimplemented)

## Compression levels and methods

- [x] Levels `0..9` (matcher profile selection)
- [x] Small-string match compression
- [x] Source-copy delta compression
- [x] Target-copy compression
- [x] Run-length encoding (`RUN`)
- [x] Undocumented soft matcher tuning (`-C`, 7 fields)
- [x] Disable target-side compression (`-N` / `XD3_NOCOMPRESS`)

## Streaming capabilities

- [x] Re-entrant stream state machine
- [x] Incremental input via `xd3_avail_input`
- [x] Incremental output via `XD3_OUTPUT` + `xd3_consume_output`
- [x] Async source-block fetch option (`XD3_GETSRCBLK`)
- [x] CLI windowed processing with EOF flush

## Buffer management

- [x] Tunable input window (`-W`)
- [x] Tunable source window (`-B`)
- [x] Tunable IOPT queue (`-I`)
- [x] Tunable duplicate-chain memory (`-P`)
- [x] Paged output section buffers (`XD3_ALLOCSIZE`)
- [x] Source block LRU/FIFO cache (`MAX_LRU_SIZE`)
- [x] Zero-copy decoder section path when possible

## External compression support

- [x] Auto-detect compressed input by magic bytes
- [x] External decompression subprocess pipeline
- [x] External recompression on output
- [x] Supported external formats: `gzip`, `bzip2`, `compress`, `xz`
- [x] Flags `-D`, `-R`, `-F` for control
- [ ] Guaranteed bit-identical recompression output (not guaranteed)

## Secondary compression

- [x] DJW static/semi-adaptive Huffman
- [x] FGK adaptive Huffman
- [x] LZMA secondary
- [x] Per-section enable/disable (`DATA/INST/ADDR`)
- [x] Secondary section delta-indicator bits (`VCD_DATACOMP`, etc.)
- [ ] Standardized IANA secondary IDs (uses project-specific IDs)

## Checksum options

- [x] Adler-32 window checksum emit/verify
- [x] Disable checksum behavior via `-n` (`XD3_ADLER32_NOVER` on decode)
- [ ] MD5
- [ ] SHA-1
- [ ] SHA-256

---

## C-Code Bottlenecks and Optimization Opportunities

Priority is based on likely runtime impact in typical large-file delta workloads.

## High Priority

1. Source checksum indexing loop (`xd3_srcwin_move_point`)

- Why it hurts: heavy per-byte/per-step rolling checksum work + frequent source block fetch checks.
- Evidence: source comments mark it as one of the most expensive and critical paths (`xdelta3.c:4327`).
- Opportunities:
  - Specialize/inline `large_look` and `large_step` for active matcher profile.
  - SIMD/vectorized checksum kernels.
  - Better move-point policy (currently admits being arbitrary in comments).
  - Reduce hash insert pressure with adaptive sampling under low match rates.

2. Small-match candidate expansion (`xd3_smatch`)

- Why it hurts: byte-by-byte compare and linked-chain traversal in hot loop.
- Opportunities:
  - Replace byte-at-a-time compare with word/SIMD compare where safe.
  - Tighten candidate pruning using address-cost estimate earlier.
  - Consider secondary hash to cut false positives before full compare.

3. Source match extension (`xd3_source_extend_match`)

- Why it hurts: backward extension uses scalar loops and may cross many block boundaries with repeated `xd3_getblk`.
- Opportunities:
  - Chunked backward compare similar to existing forward chunk path.
  - Keep more contiguous source mapped to reduce `getblk` churn.
  - Rework TOOFARBACK handling to avoid repeated failed probes.

4. Decoder output execution (`xd3_decode_output_halfinst`)

- Why it hurts: called for every instruction and performs all copies/adds/runs.
- Opportunities:
  - Use `memmove` fast path for overlapping target copies (instead of manual byte loop).
  - Fuse checksum computation with output-copy loop when checksum verification is enabled.
  - Special-case very common instruction patterns for branch reduction.

## Medium Priority

5. IOPT overlap compaction (`xd3_iopt_flush_instructions`)

- Why it hurts: list-walk overlap resolution is branchy and can degrade with many candidates.
- Opportunities:
  - Convert active queue to contiguous array for cache locality at flush time.
  - Introduce scoring model that includes address cache cost, not only size overlap.
  - Consider bounded DP for dense-overlap regions.

6. Window hash-table reset cost

- Why it hurts: `small_table` memset each window can be expensive at large table sizes.
- Opportunities:
  - Generation-tagged table entries to avoid full clears.
  - Keep absolute offsets (comment already notes this as a possible improvement).

7. Secondary DJW/FGK runtime

- Why it hurts: deeply nested loops, many passes, bit-level encode/decode, adaptive/group rebuild cost.
- Opportunities:
  - Table-driven decode in DJW (already hinted in code comments).
  - Earlier inefficiency bailout.
  - Parallelize section compression (`DATA`, `INST`, `ADDR`) when latency matters.

8. External compression process overhead

- Why it hurts: `fork/pipe/exec` overhead dominates small and medium files.
- Opportunities:
  - Optional in-process codec libraries for gzip/bzip2/xz paths.
  - Reuse worker subprocess for batch operations.

## Low Priority / Correctness-Adjacent Findings

9. Secondary compression logging condition

- `main_set_secondary_flags` prints selected algorithm using bitwise-OR checks, which can misreport.
- Low runtime impact, but confusing observability.

10. CLI level-to-LZMA preset propagation

- LZMA secondary init reads `XD3_COMPLEVEL_*` flags, but CLI path primarily maps levels to matcher config and does not set those bits.
- Potential compression-ratio/speed tuning mismatch for `-S lzma`.

---

## Summary

The repository contains a mature and heavily optimized VCDIFF encoder/decoder with a rich CLI wrapper and optional secondary/external compression support. The core architecture is robust and re-entrant, with careful overflow checks and bounded window decoding, but there are known performance hotspots in match indexing/extension, decoder output execution, and secondary compressors. The biggest practical optimization gains are likely in `xd3_srcwin_move_point`, `xd3_smatch`, and `xd3_decode_output_halfinst`.
