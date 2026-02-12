use axum::{response::Html, routing::get, Router};
use clap::Parser;
use color_eyre::Result;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Console - Web interface for music management"
)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "3000")]
    port: u16,
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>MDMA Console</title>
    <style>
        body {
            font-family: system-ui, -apple-system, sans-serif;
            max-width: 800px;
            margin: 50px auto;
            padding: 20px;
            background: #1a1a2e;
            color: #eee;
        }
        h1 { color: #00d9ff; }
        .status {
            background: #16213e;
            padding: 20px;
            border-radius: 8px;
            margin: 20px 0;
        }
        .coming-soon {
            color: #888;
            font-style: italic;
        }
    </style>
</head>
<body>
    <h1>MDMA Console</h1>
    <div class="status">
        <h2>System Status</h2>
        <p>Console is running.</p>
    </div>
    <div class="status">
        <h2>Library</h2>
        <p class="coming-soon">Library browsing coming soon...</p>
    </div>
    <div class="status">
        <h2>Now Playing</h2>
        <p class="coming-soon">Playback status coming soon...</p>
    </div>
</body>
</html>"#,
    )
}

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_console=info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health));

    let addr = format!("0.0.0.0:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("MDMA Console listening on http://0.0.0.0:{}", args.port);

    axum::serve(listener, app).await?;

    Ok(())
}
