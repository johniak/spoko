fn main() {
    std::process::exit(spoko::cli::main_with(std::env::args_os(), &mut std::io::stdout()));
}
