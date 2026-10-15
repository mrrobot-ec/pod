fn main() {
    if let Err(error) = pod::cli::run() {
        let code = pod::cli::output::emit_err(pod::cli::json_requested(), &error);
        std::process::exit(code);
    }
}
