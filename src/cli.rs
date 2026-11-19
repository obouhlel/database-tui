use std::path::PathBuf;

use clap::Parser;

/// Keyboard-driven PostgreSQL client for the terminal.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Connect directly to this connection from config.toml.
    #[arg(short, long, value_name = "NAME", conflicts_with = "url")]
    pub connection: Option<String>,

    /// Connect directly to a URL (`postgres://user@host:port/db?sslmode=...`).
    /// `DATABASE_URL`, when set, is offered in the picker instead.
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Use another configuration file.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Validate the configuration and exit.
    #[arg(long)]
    pub check_config: bool,

    /// Test the connections of the configuration (all, or the ones named)
    /// and exit; the exit code is 1 when one of them fails.
    #[arg(long, value_name = "NAME", num_args = 0.., conflicts_with_all = ["url", "connection"])]
    pub check_connections: Option<Vec<String>>,
}
