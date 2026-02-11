#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut args = Vec::<String>::new();
    for token in text.split_whitespace().take(32) {
        args.push(token.to_string());
    }
    oxidelta::cli::fuzz_try_parse_args(&args);
});
