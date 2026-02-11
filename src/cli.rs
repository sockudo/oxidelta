// Idiomatic Rust CLI for Oxidelta.
//
// Uses explicit subcommands and long-form options while preserving
// the underlying encode/decode/recode/merge behavior.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum, ValueHint};

use crate::compress::decoder::DeltaDecoder;
use crate::compress::encoder::{CompressOptions, DeltaEncoder};
use crate::compress::secondary::SecondaryCompression;
use crate::vcdiff::Instruction;
use crate::vcdiff::decoder::InstructionIterator;
use crate::vcdiff::header::{
    self, FileHeader, VCD_ADDRCOMP, VCD_ADLER32, VCD_APPHEADER, VCD_CODETABLE, VCD_DATACOMP,
    VCD_INSTCOMP, VCD_SECONDARY, VCD_SOURCE, VCD_TARGET, WindowHeader,
};

// ---------------------------------------------------------------------------
// Constants (matching xdelta3 defaults)
// ---------------------------------------------------------------------------

const XD3_DEFAULT_LEVEL: u32 = 6;
const XD3_DEFAULT_WINSIZE: usize = 1 << 23; // 8 MiB
const XD3_DEFAULT_SRCWINSZ: u64 = 1 << 26; // 64 MiB
const XD3_DEFAULT_IOPT_SIZE: usize = 1 << 15; // 32 KiB
const XD3_DEFAULT_SPREVSZ: usize = 1 << 18; // 256 KiB
const XD3_HARDMAXWINSIZE: usize = 1 << 24; // 16 MiB

const BUF_SIZE: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Byte size parsing (supports K, M, G suffixes)
// ---------------------------------------------------------------------------

fn parse_byte_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size string".into());
    }
    let (num_part, multiplier) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 1024u64),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1u64),
    };
    let num: u64 = num_part
        .trim()
        .parse()
        .map_err(|e| format!("invalid size '{s}': {e}"))?;
    num.checked_mul(multiplier)
        .ok_or_else(|| format!("size overflow: '{s}'"))
}

// ---------------------------------------------------------------------------
// Clap CLI definition
// ---------------------------------------------------------------------------

/// VCDIFF (RFC 3284) delta encoder/decoder.
#[derive(Parser, Debug)]
#[command(
    name = "oxidelta",
    version,
    about = "VCDIFF delta encoder/decoder",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,

    /// Force overwrite existing output files.
    #[arg(short = 'f', long, global = true)]
    force: bool,

    /// Quiet mode (suppress non-error output).
    #[arg(short = 'q', long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Verbose mode (use multiple times for more detail).
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    verbose: u8,

    /// Output stats as JSON to stderr.
    #[arg(long = "json", global = true)]
    json_output: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Delta encode an input stream.
    Encode(EncodeArgs),
    /// Delta decode an input stream.
    Decode(DecodeArgs),
    /// Print build/configuration details.
    Config,
    /// Print information about the first VCDIFF window.
    Header(PrintArgs),
    /// Print information about all VCDIFF windows.
    Headers(PrintArgs),
    /// Print entire delta information (headers + instructions).
    Delta(PrintArgs),
    /// Re-encode a VCDIFF file with new secondary/app-header settings.
    Recode(RecodeArgs),
    /// Merge multiple VCDIFF deltas into one.
    Merge(MergeArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SecondaryArg {
    None,
    Lzma,
    Zlib,
    Djw,
    Fgk,
}

#[derive(Args, Debug)]
struct EncodeTuningArgs {
    /// Compression level (0-9).
    #[arg(long, short = 'l', value_parser = clap::value_parser!(u32).range(0..=9), default_value_t = XD3_DEFAULT_LEVEL)]
    level: u32,

    /// Input window size (supports K/M/G suffix).
    #[arg(long = "window-size", value_parser = parse_byte_size, default_value_t = XD3_DEFAULT_WINSIZE as u64)]
    input_window_size: u64,

    /// Source window size (supports K/M/G suffix).
    #[arg(long = "source-window-size", value_parser = parse_byte_size, default_value_t = XD3_DEFAULT_SRCWINSZ)]
    source_window_size: u64,

    /// Duplicate-window tracking size (supports K/M/G suffix).
    #[arg(long = "duplicate-window-size", value_parser = parse_byte_size, default_value_t = XD3_DEFAULT_SPREVSZ as u64)]
    sprevsz: u64,

    /// Instruction optimization buffer size (supports K/M/G suffix).
    #[arg(long = "instruction-buffer-size", value_parser = parse_byte_size, default_value_t = XD3_DEFAULT_IOPT_SIZE as u64)]
    iopt_size: u64,

    /// Secondary compressor.
    #[arg(long, value_enum, default_value_t = SecondaryArg::None)]
    secondary: SecondaryArg,

    /// Disable small string-matching compression.
    #[arg(long = "disable-small-matches")]
    no_compress: bool,

    /// Disable Adler-32 checksums.
    #[arg(long = "no-checksum")]
    no_checksum: bool,
}

#[derive(Args, Debug)]
struct EncodeArgs {
    /// Source file to copy from.
    #[arg(long, short = 's', value_hint = ValueHint::FilePath)]
    source: Option<PathBuf>,

    /// Input file (default: stdin).
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "input_pos")]
    input: Option<PathBuf>,

    /// Output file (default: stdout).
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "output_pos")]
    output: Option<PathBuf>,

    /// Write output to stdout.
    #[arg(short = 'c', long)]
    stdout: bool,

    /// Check/compute only (do not write output).
    #[arg(long = "check-only")]
    no_output: bool,

    #[command(flatten)]
    tuning: EncodeTuningArgs,

    /// Input file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    input_pos: Option<PathBuf>,

    /// Output file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    output_pos: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct DecodeArgs {
    /// Source file to copy from.
    #[arg(long, short = 's', value_hint = ValueHint::FilePath)]
    source: Option<PathBuf>,

    /// Input delta file (default: stdin).
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "input_pos")]
    input: Option<PathBuf>,

    /// Output file (default: stdout).
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "output_pos")]
    output: Option<PathBuf>,

    /// Write output to stdout.
    #[arg(short = 'c', long)]
    stdout: bool,

    /// Disable Adler-32 verification.
    #[arg(long = "no-checksum")]
    no_checksum: bool,

    /// Check/compute only (do not write output).
    #[arg(long = "check-only")]
    no_output: bool,

    /// Input file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    input_pos: Option<PathBuf>,

    /// Output file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    output_pos: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct PrintArgs {
    /// VCDIFF input file.
    #[arg(value_hint = ValueHint::FilePath)]
    input: PathBuf,
}

#[derive(Args, Debug)]
struct RecodeArgs {
    /// Input VCDIFF file.
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "input_pos")]
    input: Option<PathBuf>,

    /// Output VCDIFF file (default: stdout).
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "output_pos")]
    output: Option<PathBuf>,

    /// Write output to stdout.
    #[arg(short = 'c', long)]
    stdout: bool,

    /// Secondary compressor.
    #[arg(long, value_enum, default_value_t = SecondaryArg::None)]
    secondary: SecondaryArg,

    /// Replace/attach an application header.
    #[arg(long = "app-header")]
    app_header: Option<String>,

    /// Drop the application header.
    #[arg(long = "drop-app-header", conflicts_with = "app_header")]
    drop_app_header: bool,

    /// Input file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    input_pos: Option<PathBuf>,

    /// Output file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    output_pos: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct MergeArgs {
    /// Source file to copy from.
    #[arg(long, short = 's', value_hint = ValueHint::FilePath)]
    source: Option<PathBuf>,

    /// Merge input files (repeat for each patch, in order).
    #[arg(long = "patch", short = 'p', value_name = "PATCH", value_hint = ValueHint::FilePath, action = ArgAction::Append)]
    patches: Vec<PathBuf>,

    /// Last patch input file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    last_patch: Option<PathBuf>,

    /// Output file.
    #[arg(long, value_hint = ValueHint::FilePath, conflicts_with = "output_pos")]
    output: Option<PathBuf>,

    /// Output file (positional form).
    #[arg(value_hint = ValueHint::FilePath)]
    output_pos: Option<PathBuf>,

    /// Write output to stdout.
    #[arg(short = 'c', long)]
    stdout: bool,

    #[command(flatten)]
    tuning: EncodeTuningArgs,
}

// ---------------------------------------------------------------------------
// Resolved command + options (flattened from Cli)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Encode,
    Decode,
    Config,
    PrintHdr,
    PrintHdrs,
    PrintDelta,
    Recode,
    Merge,
}

#[allow(dead_code)]
struct Options {
    command: Command,
    use_stdout: bool,
    force: bool,
    quiet: bool,
    verbose: u8,
    level: u32,
    no_compress: bool,
    no_checksum: bool,
    no_output: bool,
    use_secondary: bool,
    secondary_name: Option<String>,
    use_appheader: bool,
    appheader: Option<String>,
    source_window_size: u64,
    input_window_size: usize,
    iopt_size: usize,
    sprevsz: usize,
    source_file: Option<PathBuf>,
    input_file: Option<PathBuf>,
    output_file: Option<PathBuf>,
    merge_files: Vec<PathBuf>,
    json_output: bool,
}

fn secondary_name(sec: SecondaryArg) -> Option<String> {
    match sec {
        SecondaryArg::None => None,
        SecondaryArg::Lzma => Some("lzma".to_string()),
        SecondaryArg::Zlib => Some("zlib".to_string()),
        SecondaryArg::Djw => Some("djw".to_string()),
        SecondaryArg::Fgk => Some("fgk".to_string()),
    }
}

fn resolve_options(cli: Cli) -> Options {
    let quiet = cli.quiet;
    let verbose = cli.verbose.min(2);
    let force = cli.force;
    let json_output = cli.json_output;

    match cli.command {
        Cmd::Encode(args) => {
            let secondary_name = secondary_name(args.tuning.secondary);
            Options {
                command: Command::Encode,
                use_stdout: args.stdout,
                force,
                quiet,
                verbose,
                level: args.tuning.level,
                no_compress: args.tuning.no_compress,
                no_checksum: args.tuning.no_checksum,
                no_output: args.no_output,
                use_secondary: secondary_name.is_some(),
                secondary_name,
                use_appheader: true,
                appheader: None,
                source_window_size: args.tuning.source_window_size,
                input_window_size: args.tuning.input_window_size as usize,
                iopt_size: args.tuning.iopt_size as usize,
                sprevsz: args.tuning.sprevsz as usize,
                source_file: args.source,
                input_file: args.input.or(args.input_pos),
                output_file: args.output.or(args.output_pos),
                merge_files: Vec::new(),
                json_output,
            }
        }
        Cmd::Decode(args) => Options {
            command: Command::Decode,
            use_stdout: args.stdout,
            force,
            quiet,
            verbose,
            level: XD3_DEFAULT_LEVEL,
            no_compress: false,
            no_checksum: args.no_checksum,
            no_output: args.no_output,
            use_secondary: false,
            secondary_name: None,
            use_appheader: true,
            appheader: None,
            source_window_size: XD3_DEFAULT_SRCWINSZ,
            input_window_size: XD3_DEFAULT_WINSIZE,
            iopt_size: XD3_DEFAULT_IOPT_SIZE,
            sprevsz: XD3_DEFAULT_SPREVSZ,
            source_file: args.source,
            input_file: args.input.or(args.input_pos),
            output_file: args.output.or(args.output_pos),
            merge_files: Vec::new(),
            json_output,
        },
        Cmd::Config => Options {
            command: Command::Config,
            use_stdout: false,
            force,
            quiet,
            verbose,
            level: XD3_DEFAULT_LEVEL,
            no_compress: false,
            no_checksum: false,
            no_output: false,
            use_secondary: false,
            secondary_name: None,
            use_appheader: true,
            appheader: None,
            source_window_size: XD3_DEFAULT_SRCWINSZ,
            input_window_size: XD3_DEFAULT_WINSIZE,
            iopt_size: XD3_DEFAULT_IOPT_SIZE,
            sprevsz: XD3_DEFAULT_SPREVSZ,
            source_file: None,
            input_file: None,
            output_file: None,
            merge_files: Vec::new(),
            json_output,
        },
        Cmd::Header(args) => Options {
            command: Command::PrintHdr,
            use_stdout: false,
            force,
            quiet,
            verbose,
            level: XD3_DEFAULT_LEVEL,
            no_compress: false,
            no_checksum: false,
            no_output: false,
            use_secondary: false,
            secondary_name: None,
            use_appheader: true,
            appheader: None,
            source_window_size: XD3_DEFAULT_SRCWINSZ,
            input_window_size: XD3_DEFAULT_WINSIZE,
            iopt_size: XD3_DEFAULT_IOPT_SIZE,
            sprevsz: XD3_DEFAULT_SPREVSZ,
            source_file: None,
            input_file: Some(args.input),
            output_file: None,
            merge_files: Vec::new(),
            json_output,
        },
        Cmd::Headers(args) => Options {
            command: Command::PrintHdrs,
            use_stdout: false,
            force,
            quiet,
            verbose,
            level: XD3_DEFAULT_LEVEL,
            no_compress: false,
            no_checksum: false,
            no_output: false,
            use_secondary: false,
            secondary_name: None,
            use_appheader: true,
            appheader: None,
            source_window_size: XD3_DEFAULT_SRCWINSZ,
            input_window_size: XD3_DEFAULT_WINSIZE,
            iopt_size: XD3_DEFAULT_IOPT_SIZE,
            sprevsz: XD3_DEFAULT_SPREVSZ,
            source_file: None,
            input_file: Some(args.input),
            output_file: None,
            merge_files: Vec::new(),
            json_output,
        },
        Cmd::Delta(args) => Options {
            command: Command::PrintDelta,
            use_stdout: false,
            force,
            quiet,
            verbose,
            level: XD3_DEFAULT_LEVEL,
            no_compress: false,
            no_checksum: false,
            no_output: false,
            use_secondary: false,
            secondary_name: None,
            use_appheader: true,
            appheader: None,
            source_window_size: XD3_DEFAULT_SRCWINSZ,
            input_window_size: XD3_DEFAULT_WINSIZE,
            iopt_size: XD3_DEFAULT_IOPT_SIZE,
            sprevsz: XD3_DEFAULT_SPREVSZ,
            source_file: None,
            input_file: Some(args.input),
            output_file: None,
            merge_files: Vec::new(),
            json_output,
        },
        Cmd::Recode(args) => {
            let secondary_name = secondary_name(args.secondary);
            let (use_appheader, appheader) = if args.drop_app_header {
                (false, None)
            } else if let Some(app) = args.app_header {
                (true, Some(app))
            } else {
                (true, None)
            };
            Options {
                command: Command::Recode,
                use_stdout: args.stdout,
                force,
                quiet,
                verbose,
                level: XD3_DEFAULT_LEVEL,
                no_compress: false,
                no_checksum: false,
                no_output: false,
                use_secondary: secondary_name.is_some(),
                secondary_name,
                use_appheader,
                appheader,
                source_window_size: XD3_DEFAULT_SRCWINSZ,
                input_window_size: XD3_DEFAULT_WINSIZE,
                iopt_size: XD3_DEFAULT_IOPT_SIZE,
                sprevsz: XD3_DEFAULT_SPREVSZ,
                source_file: None,
                input_file: args.input.or(args.input_pos),
                output_file: args.output.or(args.output_pos),
                merge_files: Vec::new(),
                json_output,
            }
        }
        Cmd::Merge(args) => {
            let secondary_name = secondary_name(args.tuning.secondary);
            Options {
                command: Command::Merge,
                use_stdout: args.stdout,
                force,
                quiet,
                verbose,
                level: args.tuning.level,
                no_compress: args.tuning.no_compress,
                no_checksum: args.tuning.no_checksum,
                no_output: false,
                use_secondary: secondary_name.is_some(),
                secondary_name,
                use_appheader: true,
                appheader: None,
                source_window_size: args.tuning.source_window_size,
                input_window_size: args.tuning.input_window_size as usize,
                iopt_size: args.tuning.iopt_size as usize,
                sprevsz: args.tuning.sprevsz as usize,
                source_file: args.source,
                input_file: args.last_patch,
                output_file: args.output.or(args.output_pos),
                merge_files: args.patches,
                json_output,
            }
        }
    }
}

#[cfg(any(test, feature = "fuzzing"))]
pub fn fuzz_try_parse_args(args: &[String]) {
    let argv: Vec<String> = std::iter::once("oxidelta".to_string())
        .chain(args.iter().cloned())
        .collect();
    if let Ok(cli) = Cli::try_parse_from(argv) {
        let _ = resolve_options(cli);
    }
}

// ---------------------------------------------------------------------------
// Config command
// ---------------------------------------------------------------------------

fn cmd_config() -> i32 {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("oxidelta version {version} (Rust), Copyright (C) oxidelta contributors");
    eprintln!("Licensed under the Apache License, Version 2.0");

    let lzma = cfg!(feature = "lzma-secondary") as u8;
    let zlib = cfg!(feature = "zlib-secondary") as u8;
    let adler32 = cfg!(feature = "adler32") as u8;
    let file_io = cfg!(feature = "file-io") as u8;
    let ptr_size = std::mem::size_of::<*const ()>();

    eprintln!("SECONDARY_LZMA={lzma}");
    eprintln!("SECONDARY_ZLIB={zlib}");
    eprintln!("ADLER32={adler32}");
    eprintln!("FILE_IO={file_io}");
    eprintln!("XD3_DEFAULT_LEVEL={XD3_DEFAULT_LEVEL}");
    eprintln!("XD3_DEFAULT_IOPT_SIZE={XD3_DEFAULT_IOPT_SIZE}");
    eprintln!("XD3_DEFAULT_SPREVSZ={XD3_DEFAULT_SPREVSZ}");
    eprintln!("XD3_DEFAULT_SRCWINSZ={XD3_DEFAULT_SRCWINSZ}");
    eprintln!("XD3_DEFAULT_WINSIZE={XD3_DEFAULT_WINSIZE}");
    eprintln!("XD3_HARDMAXWINSIZE={XD3_HARDMAXWINSIZE}");
    eprintln!("sizeof(usize)={ptr_size}");

    0
}

// ---------------------------------------------------------------------------
// Build CompressOptions from CLI options
// ---------------------------------------------------------------------------

fn build_compress_options(opts: &Options) -> CompressOptions {
    let secondary = if opts.use_secondary {
        match opts.secondary_name.as_deref() {
            #[cfg(feature = "lzma-secondary")]
            Some("lzma") => SecondaryCompression::Lzma,
            #[cfg(feature = "zlib-secondary")]
            Some("zlib") => SecondaryCompression::Zlib { level: opts.level },
            Some(name) => {
                eprintln!("oxidelta: warning: unknown secondary compressor '{name}', using none");
                SecondaryCompression::None
            }
            None => {
                #[cfg(feature = "lzma-secondary")]
                {
                    SecondaryCompression::Lzma
                }
                #[cfg(not(feature = "lzma-secondary"))]
                {
                    SecondaryCompression::None
                }
            }
        }
    } else {
        SecondaryCompression::None
    };

    CompressOptions {
        level: opts.level,
        window_size: opts.input_window_size,
        checksum: !opts.no_checksum,
        secondary,
    }
}

// ---------------------------------------------------------------------------
// Encode command
// ---------------------------------------------------------------------------

fn cmd_encode(opts: &Options) -> i32 {
    let compress_opts = build_compress_options(opts);

    // Read source file (if any) fully into memory.
    let source = match &opts.source_file {
        Some(path) => match std::fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("oxidelta: source file: {}: {e}", path.display());
                return 1;
            }
        },
        None => Vec::new(),
    };

    // Open input (target): file or stdin.
    let target_reader: Box<dyn Read> = match &opts.input_file {
        Some(path) => match File::open(path) {
            Ok(f) => Box::new(BufReader::with_capacity(BUF_SIZE, f)),
            Err(e) => {
                eprintln!("oxidelta: input file: {}: {e}", path.display());
                return 1;
            }
        },
        None => Box::new(BufReader::new(io::stdin())),
    };

    // Open output: file or stdout.
    let output_writer: Box<dyn Write> = match (opts.use_stdout, &opts.output_file) {
        (true, _) | (_, None) => Box::new(BufWriter::with_capacity(BUF_SIZE, io::stdout().lock())),
        (false, Some(path)) => {
            if path.exists() && !opts.force {
                eprintln!(
                    "oxidelta: output file exists, use -f to overwrite: {}",
                    path.display()
                );
                return 1;
            }
            match File::create(path) {
                Ok(f) => Box::new(BufWriter::with_capacity(BUF_SIZE, f)),
                Err(e) => {
                    eprintln!("oxidelta: output file: {}: {e}", path.display());
                    return 1;
                }
            }
        }
    };

    if opts.no_output {
        let mut reader = target_reader;
        let mut buf = vec![0u8; BUF_SIZE];
        let mut total = 0u64;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => total += n as u64,
                Err(e) => {
                    eprintln!("oxidelta: read error: {e}");
                    return 1;
                }
            }
        }
        if !opts.quiet {
            eprintln!("oxidelta: input size: {total}");
        }
        return 0;
    }

    let mut encoder = DeltaEncoder::new(output_writer, &source, compress_opts);
    let mut reader = target_reader;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut total_in = 0u64;

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                total_in += n as u64;
                if let Err(e) = encoder.write_target(&buf[..n]) {
                    eprintln!("oxidelta: encode error: {e}");
                    return 1;
                }
            }
            Err(e) => {
                eprintln!("oxidelta: read error: {e}");
                return 1;
            }
        }
    }

    let (mut writer, windows) = match encoder.finish() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("oxidelta: encode finish error: {e}");
            return 1;
        }
    };

    if let Err(e) = writer.flush() {
        eprintln!("oxidelta: write flush error: {e}");
        return 1;
    }

    if opts.verbose > 0 && !opts.quiet {
        let source_size = source.len() as u64;
        eprintln!(
            "oxidelta: encoder: source size: {source_size}, input size: {total_in}, \
             windows: {windows}"
        );
    }

    if opts.json_output {
        let source_size = source.len() as u64;
        let json = serde_json::json!({
            "command": "encode",
            "source_size": source_size,
            "input_size": total_in,
            "windows": windows,
            "level": opts.level,
        });
        eprintln!("{}", serde_json::to_string_pretty(&json).unwrap());
    }

    0
}

// ---------------------------------------------------------------------------
// Decode command
// ---------------------------------------------------------------------------

fn cmd_decode(opts: &Options) -> i32 {
    let source = match &opts.source_file {
        Some(path) => match std::fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("oxidelta: source file: {}: {e}", path.display());
                return 1;
            }
        },
        None => Vec::new(),
    };

    let delta_reader: Box<dyn Read> = match &opts.input_file {
        Some(path) => match File::open(path) {
            Ok(f) => Box::new(BufReader::with_capacity(BUF_SIZE, f)),
            Err(e) => {
                eprintln!("oxidelta: input file: {}: {e}", path.display());
                return 1;
            }
        },
        None => Box::new(BufReader::new(io::stdin())),
    };

    let mut output_writer: Box<dyn Write> = if opts.no_output {
        Box::new(io::sink())
    } else if opts.use_stdout || opts.output_file.is_none() {
        Box::new(BufWriter::with_capacity(BUF_SIZE, io::stdout().lock()))
    } else {
        let path = opts.output_file.as_ref().unwrap();
        if path.exists() && !opts.force {
            eprintln!(
                "oxidelta: output file exists, use -f to overwrite: {}",
                path.display()
            );
            return 1;
        }
        match File::create(path) {
            Ok(f) => Box::new(BufWriter::with_capacity(BUF_SIZE, f)),
            Err(e) => {
                eprintln!("oxidelta: output file: {}: {e}", path.display());
                return 1;
            }
        }
    };

    let verify_checksum = !opts.no_checksum;
    let mut decoder = DeltaDecoder::with_checksum(delta_reader, verify_checksum);
    let mut src: &[u8] = &source;

    match decoder.decode_to(&mut src, &mut output_writer) {
        Ok(total) => {
            if let Err(e) = output_writer.flush() {
                eprintln!("oxidelta: write flush error: {e}");
                return 1;
            }
            if opts.verbose > 0 && !opts.quiet {
                let windows = decoder.windows_decoded();
                eprintln!("oxidelta: decoder: output size: {total}, windows: {windows}");
            }
            if opts.json_output {
                let windows = decoder.windows_decoded();
                let json = serde_json::json!({
                    "command": "decode",
                    "output_size": total,
                    "windows": windows,
                });
                eprintln!("{}", serde_json::to_string_pretty(&json).unwrap());
            }
        }
        Err(e) => {
            eprintln!("oxidelta: decode error: {e}");
            return 1;
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Print commands (printhdr, printhdrs, printdelta)
// ---------------------------------------------------------------------------

fn cmd_print(opts: &Options) -> i32 {
    let input_file = match &opts.input_file {
        Some(path) => path.clone(),
        None => {
            eprintln!("oxidelta: print commands require an input file");
            return 1;
        }
    };

    let file = match File::open(&input_file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("oxidelta: {}: {e}", input_file.display());
            return 1;
        }
    };
    let mut reader = BufReader::with_capacity(BUF_SIZE, file);

    let file_hdr = match FileHeader::decode(&mut reader) {
        Ok(hdr) => hdr,
        Err(e) => {
            eprintln!("oxidelta: invalid VCDIFF header: {e}");
            return 1;
        }
    };

    // Compute header size: magic(4) + hdr_ind(1) + optional fields.
    let mut hdr_size: usize = 5;
    if file_hdr.hdr_ind & VCD_SECONDARY != 0 {
        hdr_size += 1;
    }
    if file_hdr.hdr_ind & VCD_APPHEADER != 0 {
        if let Some(ref data) = file_hdr.app_header {
            hdr_size += crate::vcdiff::varint::sizeof_usize(data.len()) + data.len();
        } else {
            hdr_size += 1;
        }
    }

    println!("VCDIFF version:               0");
    println!("VCDIFF header size:           {hdr_size}");

    print!("VCDIFF header indicator:      ");
    let mut any_hdr_flag = false;
    if file_hdr.hdr_ind & VCD_SECONDARY != 0 {
        print!("VCD_SECONDARY ");
        any_hdr_flag = true;
    }
    if file_hdr.hdr_ind & VCD_CODETABLE != 0 {
        print!("VCD_CODETABLE ");
        any_hdr_flag = true;
    }
    if file_hdr.hdr_ind & VCD_APPHEADER != 0 {
        print!("VCD_APPHEADER ");
        any_hdr_flag = true;
    }
    if !any_hdr_flag {
        print!("none");
    }
    println!();

    let sec_name = match file_hdr.secondary_id {
        Some(header::VCD_LZMA_ID) => "lzma",
        Some(header::VCD_DJW_ID) => "djw",
        Some(header::VCD_FGK_ID) => "fgk",
        Some(3) => "zlib",
        Some(id) => {
            println!("VCDIFF secondary compressor:  unknown (id={id})");
            ""
        }
        None => "none",
    };
    if !sec_name.is_empty() {
        println!("VCDIFF secondary compressor:  {sec_name}");
    }

    if file_hdr.hdr_ind & VCD_APPHEADER != 0
        && let Some(ref data) = file_hdr.app_header
        && !data.is_empty()
    {
        let s = String::from_utf8_lossy(data);
        println!("VCDIFF application header:    {s}");
    }

    let mut window_num: u64 = 0;
    let mut target_offset: u64 = 0;

    loop {
        let wh = match WindowHeader::decode(&mut reader) {
            Ok(Some(wh)) => wh,
            Ok(None) => break,
            Err(e) => {
                eprintln!("oxidelta: window {window_num}: {e}");
                return 1;
            }
        };

        if window_num > 0 {
            println!();
        }

        println!("VCDIFF window number:         {window_num}");

        print!("VCDIFF window indicator:      ");
        let mut any_win_flag = false;
        if wh.win_ind & VCD_SOURCE != 0 {
            print!("VCD_SOURCE ");
            any_win_flag = true;
        }
        if wh.win_ind & VCD_TARGET != 0 {
            print!("VCD_TARGET ");
            any_win_flag = true;
        }
        if wh.win_ind & VCD_ADLER32 != 0 {
            print!("VCD_ADLER32 ");
            any_win_flag = true;
        }
        if !any_win_flag {
            print!("none");
        }
        println!();

        if let Some(cksum) = wh.adler32 {
            println!("VCDIFF adler32 checksum:      {cksum:08X}");
        }

        if wh.del_ind != 0 {
            print!("VCDIFF delta indicator:       ");
            if wh.del_ind & VCD_DATACOMP != 0 {
                print!("VCD_DATACOMP ");
            }
            if wh.del_ind & VCD_INSTCOMP != 0 {
                print!("VCD_INSTCOMP ");
            }
            if wh.del_ind & VCD_ADDRCOMP != 0 {
                print!("VCD_ADDRCOMP ");
            }
            println!();
        }

        if target_offset > 0 {
            println!("VCDIFF window at offset:      {target_offset}");
        }

        if wh.has_source() || wh.has_target() {
            println!("VCDIFF copy window length:    {}", wh.copy_window_len);
            println!("VCDIFF copy window offset:    {}", wh.copy_window_offset);
        }

        println!("VCDIFF delta encoding length: {}", wh.enc_len);
        println!("VCDIFF target window length:  {}", wh.target_window_len);
        println!("VCDIFF data section length:   {}", wh.data_len);
        println!("VCDIFF inst section length:   {}", wh.inst_len);
        println!("VCDIFF addr section length:   {}", wh.addr_len);

        if opts.command == Command::PrintDelta {
            let mut data_buf = vec![0u8; wh.data_len as usize];
            let mut inst_buf = vec![0u8; wh.inst_len as usize];
            let mut addr_buf = vec![0u8; wh.addr_len as usize];

            if let Err(e) = reader.read_exact(&mut data_buf) {
                eprintln!("oxidelta: window {window_num} data section: {e}");
                return 1;
            }
            if let Err(e) = reader.read_exact(&mut inst_buf) {
                eprintln!("oxidelta: window {window_num} inst section: {e}");
                return 1;
            }
            if let Err(e) = reader.read_exact(&mut addr_buf) {
                eprintln!("oxidelta: window {window_num} addr section: {e}");
                return 1;
            }

            let (inst_ref, addr_ref);
            let decomp_i;
            let decomp_a;
            if wh.del_ind != 0 {
                let (_, i, a) = match crate::compress::secondary::decompress_sections(
                    &data_buf,
                    &inst_buf,
                    &addr_buf,
                    wh.del_ind,
                    file_hdr.secondary_id,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("oxidelta: window {window_num} decompress: {e}");
                        return 1;
                    }
                };
                decomp_i = i;
                decomp_a = a;
                inst_ref = &decomp_i[..];
                addr_ref = &decomp_a[..];
            } else {
                inst_ref = &inst_buf;
                addr_ref = &addr_buf;
            }

            println!("  Offset Code Type1 Size1  @Addr1 + Type2 Size2 @Addr2");
            let copy_window_len = if wh.has_source() || wh.has_target() {
                wh.copy_window_len
            } else {
                0
            };

            let iter = InstructionIterator::new(inst_ref, addr_ref, copy_window_len);
            let mut offset = target_offset;
            for result in iter {
                match result {
                    Ok(inst) => match inst {
                        Instruction::Add { len } => {
                            println!("  {offset:06}     ADD  {len:6}");
                            offset += len as u64;
                        }
                        Instruction::Copy { len, addr, .. } => {
                            let addr_str = if addr >= copy_window_len {
                                format!("T@{:<6}", addr - copy_window_len)
                            } else {
                                format!("S@{:<6}", wh.copy_window_offset + addr)
                            };
                            println!("  {offset:06}     CPY  {len:6} {addr_str}");
                            offset += len as u64;
                        }
                        Instruction::Run { len } => {
                            println!("  {offset:06}     RUN  {len:6}");
                            offset += len as u64;
                        }
                    },
                    Err(e) => {
                        eprintln!("oxidelta: instruction decode: {e}");
                        return 1;
                    }
                }
            }
        } else {
            // Skip section data for printhdr/printhdrs.
            let section_total = wh.data_len as usize + wh.inst_len as usize + wh.addr_len as usize;
            let mut skip_buf = vec![0u8; section_total.min(BUF_SIZE)];
            let mut remaining = section_total;
            while remaining > 0 {
                let to_read = remaining.min(skip_buf.len());
                if let Err(e) = reader.read_exact(&mut skip_buf[..to_read]) {
                    eprintln!("oxidelta: window {window_num}: {e}");
                    return 1;
                }
                remaining -= to_read;
            }
        }

        target_offset += wh.target_window_len;
        window_num += 1;

        if opts.command == Command::PrintHdr {
            break;
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Recode command
// ---------------------------------------------------------------------------

fn cmd_recode(opts: &Options) -> i32 {
    let input_file = match &opts.input_file {
        Some(path) => path.clone(),
        None => {
            eprintln!("oxidelta: recode requires an input file");
            return 1;
        }
    };

    let file = match File::open(&input_file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("oxidelta: {}: {e}", input_file.display());
            return 1;
        }
    };
    let mut reader = BufReader::with_capacity(BUF_SIZE, file);

    let output_writer: Box<dyn Write> = match (opts.use_stdout, &opts.output_file) {
        (true, _) | (_, None) => Box::new(BufWriter::with_capacity(BUF_SIZE, io::stdout().lock())),
        (false, Some(path)) => {
            if path.exists() && !opts.force {
                eprintln!(
                    "oxidelta: output file exists, use -f to overwrite: {}",
                    path.display()
                );
                return 1;
            }
            match File::create(path) {
                Ok(f) => Box::new(BufWriter::with_capacity(BUF_SIZE, f)),
                Err(e) => {
                    eprintln!("oxidelta: output file: {}: {e}", path.display());
                    return 1;
                }
            }
        }
    };

    let in_hdr = match FileHeader::decode(&mut reader) {
        Ok(hdr) => hdr,
        Err(e) => {
            eprintln!("oxidelta: invalid VCDIFF header: {e}");
            return 1;
        }
    };

    let compress_opts = build_compress_options(opts);
    let new_secondary = compress_opts.secondary.backend();

    let mut out_hdr = FileHeader::default();
    if let Some(ref backend) = new_secondary {
        out_hdr.hdr_ind |= header::VCD_SECONDARY;
        out_hdr.secondary_id = Some(backend.id());
    }
    if opts.use_appheader {
        if let Some(ref ah) = opts.appheader {
            out_hdr.hdr_ind |= header::VCD_APPHEADER;
            out_hdr.app_header = Some(ah.as_bytes().to_vec());
        } else if let Some(ref orig_ah) = in_hdr.app_header {
            out_hdr.hdr_ind |= header::VCD_APPHEADER;
            out_hdr.app_header = Some(orig_ah.clone());
        }
    }

    let mut out_writer = output_writer;
    if let Err(e) = out_hdr.encode(&mut out_writer) {
        eprintln!("oxidelta: write header: {e}");
        return 1;
    }

    let mut window_num: u64 = 0;
    loop {
        let wh = match WindowHeader::decode(&mut reader) {
            Ok(Some(wh)) => wh,
            Ok(None) => break,
            Err(e) => {
                eprintln!("oxidelta: window {window_num}: {e}");
                return 1;
            }
        };

        let mut data_buf = vec![0u8; wh.data_len as usize];
        let mut inst_buf = vec![0u8; wh.inst_len as usize];
        let mut addr_buf = vec![0u8; wh.addr_len as usize];

        if let Err(e) = reader.read_exact(&mut data_buf) {
            eprintln!("oxidelta: window {window_num} data: {e}");
            return 1;
        }
        if let Err(e) = reader.read_exact(&mut inst_buf) {
            eprintln!("oxidelta: window {window_num} inst: {e}");
            return 1;
        }
        if let Err(e) = reader.read_exact(&mut addr_buf) {
            eprintln!("oxidelta: window {window_num} addr: {e}");
            return 1;
        }

        let (raw_data, raw_inst, raw_addr) = if wh.del_ind != 0 {
            match crate::compress::secondary::decompress_sections(
                &data_buf,
                &inst_buf,
                &addr_buf,
                wh.del_ind,
                in_hdr.secondary_id,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("oxidelta: window {window_num} decompress: {e}");
                    return 1;
                }
            }
        } else {
            (data_buf, inst_buf, addr_buf)
        };

        let (out_data, out_inst, out_addr, new_del_ind) = if let Some(ref backend) = new_secondary {
            match crate::compress::secondary::compress_sections(
                backend.as_ref(),
                &raw_data,
                &raw_inst,
                &raw_addr,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("oxidelta: window {window_num} compress: {e}");
                    return 1;
                }
            }
        } else {
            (raw_data, raw_inst, raw_addr, 0u8)
        };

        let mut new_wh = WindowHeader {
            win_ind: wh.win_ind,
            copy_window_len: wh.copy_window_len,
            copy_window_offset: wh.copy_window_offset,
            enc_len: 0,
            target_window_len: wh.target_window_len,
            del_ind: new_del_ind,
            data_len: out_data.len() as u64,
            inst_len: out_inst.len() as u64,
            addr_len: out_addr.len() as u64,
            adler32: wh.adler32,
        };
        new_wh.enc_len = new_wh.compute_enc_len();

        if let Err(e) = new_wh.encode(&mut out_writer) {
            eprintln!("oxidelta: write window header: {e}");
            return 1;
        }
        if let Err(e) = out_writer.write_all(&out_data) {
            eprintln!("oxidelta: write data: {e}");
            return 1;
        }
        if let Err(e) = out_writer.write_all(&out_inst) {
            eprintln!("oxidelta: write inst: {e}");
            return 1;
        }
        if let Err(e) = out_writer.write_all(&out_addr) {
            eprintln!("oxidelta: write addr: {e}");
            return 1;
        }

        window_num += 1;
    }

    if let Err(e) = out_writer.flush() {
        eprintln!("oxidelta: flush: {e}");
        return 1;
    }

    if opts.verbose > 0 && !opts.quiet {
        eprintln!("oxidelta: recode: {window_num} windows processed");
    }

    0
}

// ---------------------------------------------------------------------------
// Merge command
// ---------------------------------------------------------------------------

fn cmd_merge(opts: &Options) -> i32 {
    // xdelta3 merge -m 1.vcdiff -m 2.vcdiff 3.vcdiff merged.vcdiff
    // All -m files + input positional are patches applied in order.
    // Output is a single merged delta.

    let mut all_patches: Vec<PathBuf> = opts.merge_files.clone();
    if let Some(ref input) = opts.input_file {
        all_patches.push(input.clone());
    }

    if all_patches.len() < 2 {
        eprintln!("oxidelta: merge requires at least 2 patches (-m file1 ... fileN)");
        return 1;
    }

    let output_path = match &opts.output_file {
        Some(p) => Some(p.clone()),
        None if opts.use_stdout => None,
        None => {
            eprintln!("oxidelta: merge requires an output file");
            return 1;
        }
    };

    // Apply-chain: decode each patch sequentially.
    let mut current_source: Vec<u8> = Vec::new();

    for (i, patch_path) in all_patches.iter().enumerate() {
        let delta_data = match std::fs::read(patch_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("oxidelta: merge: {}: {e}", patch_path.display());
                return 1;
            }
        };

        let source_for_decode = if i == 0 {
            match &opts.source_file {
                Some(path) => match std::fs::read(path) {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("oxidelta: source file: {}: {e}", path.display());
                        return 1;
                    }
                },
                None => std::mem::take(&mut current_source),
            }
        } else {
            std::mem::take(&mut current_source)
        };

        match crate::vcdiff::decode_memory(&delta_data, &source_for_decode) {
            Ok(decoded) => {
                current_source = decoded;
            }
            Err(e) => {
                eprintln!(
                    "oxidelta: merge: patch {}: {}: {e}",
                    i + 1,
                    patch_path.display()
                );
                return 1;
            }
        }
    }

    // Re-encode: original source -> final target = merged delta.
    let original_source = match &opts.source_file {
        Some(path) => match std::fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("oxidelta: source file: {}: {e}", path.display());
                return 1;
            }
        },
        None => Vec::new(),
    };

    let final_target = &current_source;
    let compress_opts = build_compress_options(opts);

    let mut delta_output: Vec<u8> = Vec::new();
    let mut encoder = DeltaEncoder::new(&mut delta_output, &original_source, compress_opts);

    if let Err(e) = encoder.write_target(final_target) {
        eprintln!("oxidelta: merge: encode error: {e}");
        return 1;
    }
    if let Err(e) = encoder.finish() {
        eprintln!("oxidelta: merge: encode finish error: {e}");
        return 1;
    }

    if let Some(ref path) = output_path {
        if path.exists() && !opts.force {
            eprintln!(
                "oxidelta: output file exists, use -f to overwrite: {}",
                path.display()
            );
            return 1;
        }
        if let Err(e) = std::fs::write(path, &delta_output) {
            eprintln!("oxidelta: merge: write: {e}");
            return 1;
        }
    } else {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if let Err(e) = out.write_all(&delta_output) {
            eprintln!("oxidelta: merge: write: {e}");
            return 1;
        }
    }

    if opts.verbose > 0 && !opts.quiet {
        eprintln!(
            "oxidelta: merge: {} patches, output {} bytes",
            all_patches.len(),
            delta_output.len()
        );
    }

    0
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Main CLI entry point. Parses arguments via clap, dispatches commands.
pub fn run() -> ! {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let cli = Cli::parse();
    let mut opts = resolve_options(cli);

    // Validate -W against hard max.
    if opts.input_window_size > XD3_HARDMAXWINSIZE {
        eprintln!(
            "oxidelta: -W: window size {} exceeds max {XD3_HARDMAXWINSIZE}",
            opts.input_window_size
        );
        process::exit(1);
    }

    // Warn if -c overrides output filename.
    if opts.use_stdout && opts.output_file.is_some() && !opts.quiet {
        eprintln!(
            "oxidelta: warning: -c option overrides output filename: {}",
            opts.output_file.as_ref().unwrap().display()
        );
        opts.output_file = None;
    }

    let exit_code = match opts.command {
        Command::Encode => cmd_encode(&opts),
        Command::Decode => cmd_decode(&opts),
        Command::Config => cmd_config(),
        Command::PrintHdr | Command::PrintHdrs | Command::PrintDelta => cmd_print(&opts),
        Command::Recode => cmd_recode(&opts),
        Command::Merge => cmd_merge(&opts),
    };

    process::exit(exit_code);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_opts(args: &[&str]) -> Options {
        let argv: Vec<String> = std::iter::once("oxidelta".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        let cli = Cli::try_parse_from(argv).expect("cli parse failed");
        resolve_options(cli)
    }

    #[test]
    fn parse_byte_size_suffixes() {
        assert_eq!(parse_byte_size("1").unwrap(), 1);
        assert_eq!(parse_byte_size("2K").unwrap(), 2 * 1024);
        assert_eq!(parse_byte_size("3m").unwrap(), 3 * 1024 * 1024);
        assert_eq!(parse_byte_size("4G").unwrap(), 4 * 1024 * 1024 * 1024);
        assert!(parse_byte_size("").is_err());
    }

    #[test]
    fn encode_subcommand_maps_correctly() {
        let opts = parse_opts(&[
            "encode",
            "--source",
            "source.bin",
            "--level",
            "9",
            "--window-size",
            "8M",
            "--secondary",
            "lzma",
            "in.bin",
            "out.vcdiff",
        ]);
        assert_eq!(opts.command, Command::Encode);
        assert_eq!(opts.level, 9);
        assert_eq!(opts.input_window_size, 8 * 1024 * 1024);
        assert_eq!(
            opts.source_file.as_deref(),
            Some(std::path::Path::new("source.bin"))
        );
        assert_eq!(opts.input_file, Some(PathBuf::from("in.bin")));
        assert_eq!(opts.output_file, Some(PathBuf::from("out.vcdiff")));
        assert!(opts.use_secondary);
        assert_eq!(opts.secondary_name.as_deref(), Some("lzma"));
    }

    #[test]
    fn decode_subcommand_maps_correctly() {
        let opts = parse_opts(&[
            "--quiet",
            "decode",
            "--source",
            "source.bin",
            "--no-checksum",
            "--check-only",
            "in.vcdiff",
            "out.bin",
        ]);
        assert_eq!(opts.command, Command::Decode);
        assert!(opts.no_checksum);
        assert!(opts.no_output);
        assert!(opts.quiet);
        assert_eq!(
            opts.source_file.as_deref(),
            Some(std::path::Path::new("source.bin"))
        );
        assert_eq!(opts.input_file, Some(PathBuf::from("in.vcdiff")));
        assert_eq!(opts.output_file, Some(PathBuf::from("out.bin")));
    }

    #[test]
    fn global_stdio_and_force_flags() {
        let opts = parse_opts(&["--force", "encode", "--stdout", "in", "out"]);
        assert!(opts.use_stdout);
        assert!(opts.force);
    }

    #[test]
    fn verbose_is_capped() {
        let verbose = parse_opts(&["--verbose", "--verbose", "--verbose", "encode", "in", "out"]);
        assert_eq!(verbose.verbose, 2);
    }

    #[test]
    fn tuning_flags_parse() {
        let opts = parse_opts(&[
            "encode",
            "--source-window-size",
            "64M",
            "--window-size",
            "8M",
            "--duplicate-window-size",
            "256K",
            "--instruction-buffer-size",
            "32K",
            "--disable-small-matches",
            "--no-checksum",
            "in",
            "out",
        ]);
        assert_eq!(opts.source_window_size, 64 * 1024 * 1024);
        assert_eq!(opts.input_window_size, 8 * 1024 * 1024);
        assert_eq!(opts.sprevsz, 256 * 1024);
        assert_eq!(opts.iopt_size, 32 * 1024);
        assert!(opts.no_compress);
        assert!(opts.no_checksum);
    }

    #[test]
    fn recode_app_header_flags() {
        let enabled = parse_opts(&["recode", "--app-header", "hello", "in", "out"]);
        assert!(enabled.use_appheader);
        assert_eq!(enabled.appheader.as_deref(), Some("hello"));

        let dropped = parse_opts(&["recode", "--drop-app-header", "in", "out"]);
        assert!(!dropped.use_appheader);
        assert!(dropped.appheader.is_none());
    }

    #[test]
    fn merge_flags_parse() {
        let opts = parse_opts(&[
            "merge",
            "--patch",
            "a.vcdiff",
            "--patch",
            "b.vcdiff",
            "c.vcdiff",
            "out.vcdiff",
        ]);
        assert_eq!(opts.command, Command::Merge);
        assert_eq!(
            opts.merge_files,
            vec![PathBuf::from("a.vcdiff"), PathBuf::from("b.vcdiff")]
        );
        assert_eq!(opts.input_file, Some(PathBuf::from("c.vcdiff")));
        assert_eq!(opts.output_file, Some(PathBuf::from("out.vcdiff")));
    }

    #[test]
    fn header_commands_map() {
        assert_eq!(parse_opts(&["header", "in"]).command, Command::PrintHdr);
        assert_eq!(parse_opts(&["headers", "in"]).command, Command::PrintHdrs);
        assert_eq!(parse_opts(&["delta", "in"]).command, Command::PrintDelta);
    }

    #[test]
    fn config_command_maps() {
        assert_eq!(parse_opts(&["config"]).command, Command::Config);
    }

    #[test]
    fn compress_options_mapping() {
        let opts = parse_opts(&[
            "encode",
            "--level",
            "6",
            "--window-size",
            "1M",
            "--no-checksum",
            "--secondary",
            "none",
            "in",
            "out",
        ]);
        let c = build_compress_options(&opts);
        assert_eq!(c.level, 6);
        assert_eq!(c.window_size, 1024 * 1024);
        assert!(!c.checksum);
        assert!(matches!(c.secondary, SecondaryCompression::None));
    }

    #[test]
    fn parse_source_and_secondary() {
        let opts = parse_opts(&[
            "encode",
            "--source",
            "source.bin",
            "--secondary",
            "lzma",
            "in",
            "out",
        ]);
        assert_eq!(
            opts.source_file.as_deref(),
            Some(std::path::Path::new("source.bin"))
        );
        assert!(opts.use_secondary);
        assert_eq!(opts.secondary_name.as_deref(), Some("lzma"));
    }
}
