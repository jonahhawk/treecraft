//! `treecraft` command-line interface.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "treecraft", version, about = "Disk space analyzer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan a directory tree (not implemented yet).
    Scan {
        /// Root directory to scan.
        root: std::path::PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { root } => {
            anyhow::bail!(
                "scan of {} is not implemented yet (milestone M1)",
                root.display()
            )
        }
    }
}
