// Server entry: native binary hosting Axum + Dioxus SSR + server functions.
#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Bind address resolution:
    // - When PORT is set (e.g. Azure Container Apps), bind 0.0.0.0:$PORT.
    // - Otherwise (local `cargo run` / `dx serve`), fall back to localhost.
    let addr: std::net::SocketAddr = match std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
    {
        Some(port) => std::net::SocketAddr::from(([0, 0, 0, 0], port)),
        None => dioxus_cli_config::fullstack_address_or_localhost(),
    };

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on {addr}");
    axum::serve(listener, {{APP_NAME_SNAKE}}::server::build_router()).await?;
    Ok(())
}

// Web entry: WASM hydration. `dx` compiles this branch for wasm32-unknown-unknown.
#[cfg(target_arch = "wasm32")]
fn main() {
    {{APP_NAME_SNAKE}}::wasm_bridge::install();
    dioxus::launch({{APP_NAME_SNAKE}}::App);
}
