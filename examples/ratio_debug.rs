use oxidelta::compress::encoder::{self, CompressOptions};
use oxidelta::compress::secondary::SecondaryCompression;
use oxidelta::vcdiff::code_table::Instruction;
use oxidelta::vcdiff::decoder::InstructionIterator;
use oxidelta::vcdiff::header::{FileHeader, WindowHeader};
use std::io::Read;

fn generate_data(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..size)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}
fn mutate_data(source: &[u8], similarity: f64, seed: u64) -> Vec<u8> {
    let mut target = source.to_vec();
    let mut state = seed;
    let change_count = ((1.0 - similarity) * source.len() as f64) as usize;
    for _ in 0..change_count {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let pos = (state >> 33) as usize % target.len();
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        target[pos] = (state >> 33) as u8;
    }
    target
}
fn decode_instructions(delta: &[u8]) -> Vec<Instruction> {
    let mut cursor = std::io::Cursor::new(delta);
    let _fh = FileHeader::decode(&mut cursor).unwrap();
    let wh = WindowHeader::decode(&mut cursor).unwrap().unwrap();
    let mut d = vec![0u8; wh.data_len as usize];
    cursor.read_exact(&mut d).unwrap();
    let mut i = vec![0u8; wh.inst_len as usize];
    cursor.read_exact(&mut i).unwrap();
    let mut a = vec![0u8; wh.addr_len as usize];
    cursor.read_exact(&mut a).unwrap();
    InstructionIterator::new(&i, &a, wh.copy_window_len)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
fn inst_pos_len(insts: &[Instruction]) -> Vec<(usize, &str, u32, Option<u64>)> {
    let mut pos = 0usize;
    let mut result = Vec::new();
    for inst in insts {
        match inst {
            Instruction::Add { len } => {
                result.push((pos, "ADD", *len, None));
                pos += *len as usize;
            }
            Instruction::Copy { len, addr, .. } => {
                result.push((pos, "CPY", *len, Some(*addr)));
                pos += *len as usize;
            }
            Instruction::Run { len } => {
                result.push((pos, "RUN", *len, None));
                pos += *len as usize;
            }
        }
    }
    result
}
fn main() {
    let source = generate_data(4096, 42);
    let target = mutate_data(&source, 0.95, 123);

    let mut rust_delta = Vec::new();
    encoder::encode_all(
        &mut rust_delta,
        &source,
        &target,
        CompressOptions {
            level: 6,
            checksum: false,
            secondary: SecondaryCompression::None,
            ..Default::default()
        },
    )
    .unwrap();
    let c_delta = xdelta3::encode(&target, &source).expect("C encode");

    let r_insts = decode_instructions(&rust_delta);
    let c_insts = decode_instructions(&c_delta);
    let r_pos = inst_pos_len(&r_insts);
    let c_pos = inst_pos_len(&c_insts);

    println!(
        "4K@95%: Rust {} bytes ({} insts), C {} bytes ({} insts)\n",
        rust_delta.len(),
        r_insts.len(),
        c_delta.len(),
        c_insts.len()
    );

    // Find first divergence point
    println!(
        "{:>5} {:>5} {:>4} {:>6} {:>6}   {:>5} {:>4} {:>6} {:>6}",
        "tpos", "R_op", "Rlen", "Raddr", "Rend", "C_op", "Clen", "Caddr", "Cend"
    );
    println!("{}", "-".repeat(70));

    let mut ri = 0;
    let mut ci = 0;
    let mut diverged = false;
    while ri < r_pos.len() || ci < c_pos.len() {
        let r = r_pos.get(ri);
        let c = c_pos.get(ci);

        let same = match (r, c) {
            (Some(r), Some(c)) => r.0 == c.0 && r.1 == c.1 && r.2 == c.2 && r.3 == c.3,
            _ => false,
        };

        if !same && !diverged {
            println!("--- DIVERGENCE POINT ---");
            diverged = true;
        }

        if diverged || ri + ci < 30 {
            let r_str = r
                .map(|(pos, op, len, addr)| {
                    let end = pos + *len as usize;
                    let addr_s = addr.map(|a| format!("{}", a)).unwrap_or("-".to_string());
                    format!("{:>5} {:>5} {:>4} {:>6} {:>6}", pos, op, len, addr_s, end)
                })
                .unwrap_or_default();
            let c_str = c
                .map(|(pos, op, len, addr)| {
                    let end = pos + *len as usize;
                    let addr_s = addr.map(|a| format!("{}", a)).unwrap_or("-".to_string());
                    format!("{:>5} {:>4} {:>6} {:>6}", op, len, addr_s, end)
                })
                .unwrap_or_default();
            let marker = if same { "" } else { " <--" };
            println!("{}   {}{}", r_str, c_str, marker);
        }

        // Advance both if same position, otherwise advance the one behind
        match (r, c) {
            (Some(r), Some(c)) if r.0 == c.0 => {
                ri += 1;
                ci += 1;
            }
            (Some(r), Some(c)) if r.0 < c.0 => {
                ri += 1;
            }
            (Some(_r), Some(_c)) => {
                ci += 1;
            }
            (Some(_), None) => {
                ri += 1;
            }
            (None, Some(_)) => {
                ci += 1;
            }
            (None, None) => break,
        }

        if diverged && ri > r_pos.len().min(ci + 20) {
            break;
        }
    }

    // Show source/target around the divergence point
    if let Some(div_pos) = r_pos
        .iter()
        .zip(c_pos.iter())
        .position(|(r, c)| r.0 != c.0 || r.1 != c.1 || r.2 != c.2 || r.3 != c.3)
    {
        let tpos = r_pos[div_pos].0;
        let start = tpos.saturating_sub(5);
        let end = (tpos + 30).min(source.len());
        println!("\nTarget bytes around divergence (pos {}):", tpos);
        for i in start..end {
            let eq = if source[i] == target[i] { "=" } else { "!" };
            if i == tpos {
                print!("[");
            }
            print!("{}{}", eq, i);
            if i == tpos {
                print!("]");
            }
            print!(" ");
        }
        println!();
        // Show what source looks like at the C match address
        if div_pos < c_pos.len()
            && let Some(addr) = c_pos[div_pos].3
        {
            println!(
                "\nC match: CPY len={} from source addr={}",
                c_pos[div_pos].2, addr
            );
            println!(
                "source[{}..{}]: matches target[{}..{}]?",
                addr,
                addr as usize + c_pos[div_pos].2 as usize,
                tpos,
                tpos + c_pos[div_pos].2 as usize
            );
            let sa = addr as usize;
            let sl = c_pos[div_pos].2 as usize;
            let mut match_count = 0;
            for i in 0..sl {
                if sa + i < source.len()
                    && tpos + i < target.len()
                    && source[sa + i] == target[tpos + i]
                {
                    match_count += 1;
                }
            }
            println!("Matching bytes: {}/{}", match_count, sl);
        }
    }
}
