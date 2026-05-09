//! Knack CLI — binary entry. Thin shell around `knack_cli::commands::dispatch`.
//!
//! Logic lives in the library so integration tests can drive it without
//! shelling out.

use std::process::ExitCode;

use clap::Parser;

use knack_cli::commands::{Command, GlobalArgs, build_client, dispatch};
use knack_cli::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "knack",
    version,
    about = "Teach the AI your job. Once.",
    long_about = "knack — author, version, share, and run AI skills. \
Run `knack docs` for the full reference."
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> ExitCode {
    // RUST_LOG=knack=debug for verbose tracing; default is silent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();
    let mode = cli.global.output_mode();
    let config = Config::load();
    let client = build_client(config, &cli.global);

    match dispatch(cli.command, client, mode).await {
        // Every error path inside `dispatch` already called `emit_err` to
        // produce the envelope; we just translate the variant to its stable
        // exit code here. POSIX caps exit at u8 (255), and our table never
        // goes that high anyway.
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let code = e.exit_code().0;
            ExitCode::from(code.clamp(1, 255) as u8)
        }
    }
}
