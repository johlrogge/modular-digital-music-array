// bases/download_cli/src/main.rs
mod app;
mod args;
mod output;

use app::App;
use args::Args;
use clap::Parser;
use color_eyre::Result;

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
