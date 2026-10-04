use std::process::ExitCode;

use clap::Parser;
use clipper_cli::{Cli, Error};
use clipper_daemon_client::Connection;
use clipper_daemon_types::ipc_path;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .init();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Error> {
    let data_dir = dirs::data_dir()
        .ok_or(Error::DataDirectory)?
        .join("Clipper");
    let mut connection = Connection::connect(&ipc_path::socket_path(), &data_dir).await?;
    cli.command
        .execute(
            &mut connection,
            std::io::stdin().lock(),
            std::io::stdout().lock(),
        )
        .await
}
