fn main() {
    if let Err(err) = nod::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
