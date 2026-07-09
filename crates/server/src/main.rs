//! Entry point for the camerata BFF. Binds the Axum app and serves until killed.
//!
//! Run it with:
//!     cargo run -p camerata-server
//! Override the bind address with CAMERATA_SERVER_ADDR (default 127.0.0.1:8787).

use tracing_subscriber::EnvFilter;

/// Install the process-global `tracing` subscriber, once (Phase H1 foundation).
///
/// Writes to STDERR — this process's stdout is not a stdio protocol channel like the
/// gateway's, but keeping BOTH binaries on the same "logs go to stderr" convention
/// means a shared log-tailing setup works uniformly across both.
///
/// Filter precedence: `CAMERATA_LOG` env var, then the conventional `RUST_LOG`, then
/// a hardcoded `"info"` default. Uses `try_init()` (not `init()`) so a double-init —
/// e.g. this binary embedding another crate that also installs a subscriber, or a
/// test harness that calls `main()` twice — is a silent no-op instead of a panic.
fn init_tracing() {
    let filter = EnvFilter::try_from_env("CAMERATA_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    // Auto-load the gitignored .env (standalone-server path).
    let _ = dotenvy::dotenv();
    let addr =
        std::env::var("CAMERATA_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    camerata_server::serve(&addr).await
}
