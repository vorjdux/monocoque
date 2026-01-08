/// Development helper: initialize tracing subscriber when `RUST_LOG` is set.
///
/// Benches and tests can call `monocoque::dev_tracing::init_tracing()` to enable
/// structured logging for debugging. This is a no-op when `RUST_LOG` is not set
/// or when a global subscriber is already installed.
pub fn init_tracing() {
    use std::env;

    if env::var("RUST_LOG").is_ok() {
        // Best-effort: try to init a fmt subscriber from env filter.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
}
