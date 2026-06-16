//! Entry point for the camerata BFF. Binds the Axum app and serves until killed.
//!
//! Run it with:
//!     cargo run -p camerata-server
//! Override the bind address with CAMERATA_SERVER_ADDR (default 127.0.0.1:8787).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Auto-load the gitignored .env (standalone-server path).
    let _ = dotenvy::dotenv();
    let addr =
        std::env::var("CAMERATA_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    camerata_server::serve(&addr).await
}
