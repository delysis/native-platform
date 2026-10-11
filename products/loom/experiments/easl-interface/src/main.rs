#![forbid(unsafe_code)]
fn main() {
    if let Err(error) = loom_easl_interface::run() {
        eprintln!("Loom EASL experiment: {error}");
        std::process::exit(1);
    }
}
