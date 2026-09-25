#![forbid(unsafe_code)]

// Compatibility launcher. The canonical Loom executable now selects the same
// chat library with --mode chat. Do not retire this entry before native parity.
fn main() {
    mom_llama_app::run();
}
