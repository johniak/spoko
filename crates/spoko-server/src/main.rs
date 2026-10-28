fn main() {
    if let Err(e) = spoko_server::cli::run_cli(std::env::args(), &mut std::io::stdout()) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
