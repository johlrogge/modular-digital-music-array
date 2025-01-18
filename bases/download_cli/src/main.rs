// bases/download_cli/src/main.rs
mod args;
mod output;
mod app;

use clap::Parser;
use color_eyre::Result;
use args::Args;
use app::App;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();
    let app = App::new(args);

    if let Err(error) = app.run().await {
        app.print_error(&error);
        std::process::exit(1);
    }
    Ok(())
}
