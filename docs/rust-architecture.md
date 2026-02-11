# Rust xdelta Architecture

## Module Structure

```
src/
  lib.rs              -- crate root, re-exports public API
  main.rs             -- CLI entry point

  vcdiff/
    mod.rs             -- VCDIFF format types, constants, RFC 3284 magic/version
    encoder.rs         -- encode state machine (ENC_INIT..ENC_POSTWIN)
    decoder.rs         -- decode state machine (DEC_VCHEAD..DEC_FINISH)
    code_table.rs      -- default RFC 3284 code table + instruction packing
    address_cache.rs   -- NEAR/SAME address mode cache
    varint.rs          -- base-128 variable-length integer encode/decode
    header.rs          -- file header and per-window header parsing/writing
    secondary.rs       -- secondary compression dispatch (DJW, FGK, LZMA)

  hash/
    mod.rs             -- trait + re-exports
    rolling.rs         -- Rabin-Karp polynomial rolling hash (large checksums)
    small.rs           -- 4-byte LCG small checksum (target-to-target)
    table.rs           -- hash table with generation tagging (avoids memset)

  compress/
    mod.rs             -- compression pipeline orchestration
    djw.rs             -- DJW static/semi-adaptive Huffman
    fgk.rs             -- FGK adaptive Huffman
    lzma.rs            -- LZMA secondary compression (via lzma-rs)
    external.rs        -- external subprocess compression (gzip, bzip2, xz)

  io/
    mod.rs             -- stream abstraction, return-code model
    source.rs          -- source block cache + getblk abstraction
    buffer.rs          -- paged output buffers, arena-style allocation
    window.rs          -- windowed input processing

  checksum/
    mod.rs             -- trait + re-exports
    adler32.rs         -- Adler-32 (window checksum, SIMD-accelerated)

  cli/
    mod.rs             -- arg parsing, command dispatch
    commands.rs        -- encode/decode/printhdr/printhdrs/printdelta/recode/merge
    config.rs          -- compression level mapping, flag definitions
    external.rs        -- external compressor detection (magic bytes) and spawning

  iopt/
    mod.rs             -- instruction optimization buffer
    overlap.rs         -- overlap resolution and pruning

  error.rs             -- unified error type (XdeltaError)
  config.rs            -- stream configuration (matcher profiles, tunables)
```

---

## Key Data Structures

### Instruction Set (zero-copy friendly)

```rust
/// Delta instruction -- kept small (8 bytes) for cache-friendly IOPT queues.
#[derive(Clone, Copy, Debug)]
pub enum Instruction {
    /// Emit literal bytes from the data section.
    Add { len: u32 },
    /// Copy `len` bytes from source or target at `addr`.
    Copy { len: u32, addr: u64, mode: AddressMode },
    /// Repeat a single byte `len` times.
    Run { len: u32, byte: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressMode {
    Self_,           // VCD_SELF (mode 0)
    Here,            // VCD_HERE (mode 1)
    Near(u8),        // NEAR cache slot (modes 2..2+s_near-1)
    Same(u8, u8),    // SAME cache slot+offset (modes 2+s_near..)
}
```

### Address Cache

```rust
pub struct AddressCache {
    near: Box<[u64]>,   // NEAR ring buffer (default 4 slots)
    same: Box<[u64]>,   // SAME table (default 3 * 256 slots)
    next_slot: usize,
}
```

### Block Hash Tables

```rust
/// Generation-tagged hash table -- avoids full memset per window.
pub struct HashTable<V: Copy> {
    entries: Box<[HashEntry<V>]>,
    generation: u32,
}

struct HashEntry<V: Copy> {
    generation: u32,
    value: V,
}

/// Small-match chain entry (target-to-target).
pub struct SmallHashTable {
    table: HashTable<u32>,       // position index by hash
    prev: Box<[u32]>,            // chain links (bounded by small_chain)
}

/// Large (source) hash table entry.
pub struct LargeHashEntry {
    checksum: u64,
    block_offset: u64,
}
```

### Stream and Window Buffers

```rust
/// Paged output buffer -- linked list of fixed-size pages.
/// Pages are recycled through a free-list to avoid repeated allocation.
pub struct OutputBuffer {
    pages: Vec<Page>,
    free_list: Vec<Page>,
    page_size: usize,            // default XD3_ALLOCSIZE (16 KiB)
}

struct Page {
    data: Box<[u8]>,
    len: usize,
}

/// Source block cache with LRU eviction.
pub struct SourceCache {
    buffer: Box<[u8]>,           // single allocation, partitioned into blocks
    block_size: u32,
    lru: [LruEntry; MAX_LRU_SIZE],
}

/// Windowed input state.
pub struct InputWindow<'a> {
    data: &'a [u8],              // zero-copy borrowed slice
    consumed: usize,
    eof: bool,
}
```

### IOPT Buffer

```rust
/// Instruction optimization buffer for the 1.5-pass encoder.
pub struct IoptBuffer {
    queue: VecDeque<PendingInstruction>,
    max_size: Option<usize>,     // None = unlimited growth
}

pub struct PendingInstruction {
    inst: Instruction,
    position: u64,               // absolute position in target
}
```

### Encoder/Decoder State

```rust
pub struct Encoder {
    config: StreamConfig,
    state: EncodeState,
    small_hash: SmallHashTable,
    large_hash: HashTable<LargeHashEntry>,
    iopt: IoptBuffer,
    sections: EncodeSections,    // data/inst/addr output buffers
    acache: AddressCache,
    src: Option<SourceCache>,
    window_count: u64,
}

pub struct Decoder {
    state: DecodeState,
    sections: DecodeSections,    // data/inst/addr input views
    acache: AddressCache,
    output: OutputBuffer,
    window_count: u64,
    verify_checksum: bool,
}
```

---

## Traits

### Hash Algorithms

```rust
/// Rolling hash used for source (large) matching.
pub trait RollingHash {
    /// Full checksum of `data[0..look]`.
    fn checksum(data: &[u8]) -> u64;

    /// Incremental update: remove `old_byte`, add `new_byte`.
    fn update(hash: u64, old_byte: u8, new_byte: u8) -> u64;
}

/// Fixed hash for small (target-to-target) matching.
pub trait SmallHash {
    fn hash(data: &[u8; 4]) -> u32;
    fn update(hash: u32, old_byte: u8, new_byte: u8) -> u32;
}
```

### Compression Backends

```rust
/// Secondary compressor (applied per-section: DATA, INST, ADDR).
pub trait SecondaryCompressor {
    fn compress(&mut self, input: &[u8], output: &mut Vec<u8>) -> Result<()>;
    fn decompress(&mut self, input: &[u8], output: &mut Vec<u8>) -> Result<()>;

    /// Hint: return false if compression would expand this section.
    fn is_worthwhile(&self, input: &[u8]) -> bool;
}
```

### I/O Streams

```rust
/// Source data provider (supports async block fetch for large sources).
pub trait SourceReader {
    /// Fetch block at `block_num`. Returns a borrowed slice valid until next call.
    fn get_block(&mut self, block_num: u64) -> Result<&[u8]>;
    fn block_size(&self) -> u32;
    fn source_len(&self) -> Option<u64>;
}

/// Abstraction over file / stdin / memory for streaming I/O.
pub trait StreamInput: std::io::Read {
    fn is_seekable(&self) -> bool;
}

pub trait StreamOutput: std::io::Write {
    fn is_seekable(&self) -> bool;
}
```

---

## Performance Plan

### SIMD Opportunities

| Hot path | Target | Approach |
|---|---|---|
| Rolling hash (`large_cksum`, `large_cksum_update`) | x86 SSE4.1 / AVX2, aarch64 NEON | Vectorized polynomial evaluation with `_mm256_mullo_epi32`; gated behind `#[cfg(target_feature)]` with scalar fallback. |
| Adler-32 checksum | x86 SSSE3+ | Use `simd-adler32` crate (auto-dispatches SIMD). |
| Small checksum 4-byte hash | All | Already a single multiply -- unlikely to benefit from SIMD; ensure inlining. |
| Match extension byte compare | x86 AVX2 | Compare 32 bytes at a time with `_mm256_cmpeq_epi8` + `_mm256_movemask_epi8`; find first mismatch with `trailing_zeros`. |
| Decoder COPY execution | All | Use `ptr::copy_nonoverlapping` / `memmove` intrinsic for non-overlapping / overlapping cases. |

### Parallel Processing

| Area | Strategy |
|---|---|
| Secondary compression of DATA/INST/ADDR sections | Each section is independent post-encoding; compress in parallel with `rayon::join` or `std::thread::scope`. |
| Multi-file batch operations | CLI can process file lists with a thread pool (encoder state is per-stream). |
| Source hash indexing | Partition source blocks across threads for initial index build; merge hash tables. Single-window encode remains sequential (match ordering is position-dependent). |

### Memory Pool / Arena Allocation

- **Output page pool**: `OutputBuffer` maintains a free-list of fixed-size pages. Freed pages return to the pool instead of being deallocated. Pages are `Box<[u8]>` to avoid `Vec` overhead.
- **IOPT arena**: when `max_size` is set, the `VecDeque` is pre-allocated to capacity. No allocations occur during steady-state encoding.
- **Hash table generation tagging**: `HashTable` uses a `generation` counter instead of `memset` to logically clear the table between windows. Only the 4-byte generation field is written per entry on lookup, avoiding a full table scan. This eliminates the O(table_size) clear cost flagged in the C analysis.
- **Source cache**: single up-front allocation partitioned into block-sized slices. LRU eviction reuses slots without allocation.

### Buffer Reuse Strategies

- **Encoder sections** (`data`, `inst`, `addr` output buffers) are `.clear()`ed between windows but retain their allocations.
- **Decoder section views** use zero-copy borrows into the input buffer when section data is contiguous (matching the C decoder's optimization).
- **Input window** borrows a slice from the caller -- no copy on ingest.
- **Varint scratch buffers** are stack-allocated (`[u8; 10]`), no heap involvement.
- **CLI file buffers** are allocated once per invocation and reused across windows via `std::io::BufReader` / `BufWriter` with configurable capacity.

---

## Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum XdeltaError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid VCDIFF header: {0}")]
    InvalidHeader(String),

    #[error("invalid window: {0}")]
    InvalidWindow(String),

    #[error("checksum mismatch: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },

    #[error("varint overflow")]
    VarintOverflow,

    #[error("window too large: {size} exceeds hard max {max}")]
    WindowTooLarge { size: u64, max: u64 },

    #[error("unsupported feature: {0}")]
    Unsupported(String),

    #[error("secondary decompression failed: {0}")]
    SecondaryDecompress(String),
}

pub type Result<T> = std::result::Result<T, XdeltaError>;
```

---

## Crate Feature Flags

```toml
[features]
default = ["cli", "adler32", "lzma-secondary"]
cli = ["dep:clap"]
adler32 = ["dep:simd-adler32"]
lzma-secondary = ["dep:lzma-rs"]
simd = []  # enable hand-written SIMD kernels (requires nightly for some targets)
```

---

## Dependency Rationale

| Crate | Purpose | Why |
|---|---|---|
| `thiserror` | Error derive macros | Zero-cost, standard ergonomic error types. |
| `clap` | CLI argument parsing | Mature, derive-based, replaces xdelta's custom parser. |
| `simd-adler32` | Adler-32 with SIMD | Drop-in, auto-dispatches SSE/AVX/NEON, matches C performance. |
| `lzma-rs` | LZMA compress/decompress | Pure-Rust LZMA for secondary compression without C dependency. |
| `log` + `env_logger` | Logging | Replaces verbosity printf with structured logging. |
| `rayon` | Parallel iterators | Optional parallelism for section compression and batch CLI. |
| `bitflags` | Flag constants | Clean representation of VCDIFF indicator bits. |

---

## Unsupported (matches C implementation)

- `VCD_TARGET` copy-window in decoder (explicitly unimplemented upstream).
- Custom `VCD_CODETABLE` decode (removed in upstream).
- Bit-identical external recompression guarantee.
