//! Shared by the benchmarks.

/// Install a log subscriber when `RUST_LOG` is set, so a benchmark can be run
/// with the library's `debug!` output. Does nothing otherwise, or when a
/// subscriber is already installed.
pub fn init_tracing() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
}
