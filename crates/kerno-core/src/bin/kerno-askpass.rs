//! The askpass program of the OpenSSH tunnel tests: what the Kerno app does
//! first thing in `main` (`kerno_core::openssh::answer_askpass`), alone.

fn main() {
    std::process::exit(kerno_core::openssh::answer_askpass().unwrap_or(1));
}
