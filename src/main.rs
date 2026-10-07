//! `main` only parses arguments and sets the process up; every decision lives in the library.
//!
//! One piece of process setup matters: on Unix the default SIGPIPE handling is restored. Rust
//! ignores SIGPIPE, so `projectlife detect <path> | head -20` would otherwise make the first write
//! after the reader leaves fail, and the program would panic with "failed printing to stdout" —
//! noisy, and wrong for a command line tool. Dying on the signal (or writing nothing more) is what
//! every other Unix program does.

fn main() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = projectlife::cli::run(args);
    std::process::exit(code);
}
