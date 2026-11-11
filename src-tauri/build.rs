fn main() {
    // The recognizer needs between 1 and 2 MiB of stack (a 1 MiB stack
    // overflows with STATUS_STACK_OVERFLOW on real Windows hardware).
    // Windows main threads reserve only 1 MiB by default, so raise the PE
    // reserve for this crate's binaries, examples and tests. Engine worker
    // threads also set an explicit stack at their spawn sites.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/STACK:8388608");
    }
    tauri_build::build()
}
