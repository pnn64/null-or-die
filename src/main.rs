fn main() {
    if let Err(err) = null_or_die::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
