//! Knack CLI — placeholder until Track E.
//!
//! Track E.1 sets up the cross-platform install + release pipeline; E.2 wires
//! the browser-based device-flow login. This binary just prints version info
//! so CI on the foundation can verify the workspace builds.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "knack", version, about = "Teach the AI your job. Once.")]
struct Cli {
    /// Print machine-readable JSON instead of human text.
    #[arg(long, global = true)]
    json: bool,
}

fn main() {
    let cli = Cli::parse();
    if cli.json {
        println!(
            r#"{{"$schema":"knack://cli/v1","ok":true,"data":{{"version":"{}","status":"placeholder"}}}}"#,
            env!("CARGO_PKG_VERSION")
        );
    } else {
        println!("knack v{} — Track E will fill this in.", env!("CARGO_PKG_VERSION"));
    }
}
