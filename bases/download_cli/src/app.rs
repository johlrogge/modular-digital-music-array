// bases/download_cli/src/app.rs
use color_eyre::Result;
use media_downloader::MediaDownloader;
use crate::args::Args;
use crate::output::OutputHandler;

pub struct App {
    args: Args,
    output: OutputHandler,
}

impl App {
    pub fn new(args: Args) -> Self {
        let output = OutputHandler::new(args.verbose);
        Self { args, output }
    }

    pub async fn run(&self) -> Result<()> {
        let downloader = MediaDownloader::new(&self.args.output_dir).await?;
        
        if self.args.playlist {
            self.output.print_playlist_download_start(&self.args.url);
            
            let results = downloader.download_playlist(&self.args.url).await?;
            
            self.output.print_playlist_download_complete(&results);
        } else {
            self.output.print_download_start(&self.args.url);
            
            let (path, metadata) = downloader.download(&self.args.url).await?;
            
            self.output.print_download_complete(&path, &metadata);
        }

        Ok(())
    }

    pub fn print_error(&self, error: &color_eyre::Report) {
        self.output.print_error(error);
    }
}
