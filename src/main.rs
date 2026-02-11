fn main() {
    #[cfg(feature = "cli")]
    oxidelta::cli::run();

    #[cfg(not(feature = "cli"))]
    {
        eprintln!("oxidelta: CLI not enabled. Rebuild with `--features cli`.");
        std::process::exit(1);
    }
}
