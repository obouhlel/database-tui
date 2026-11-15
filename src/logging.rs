//! Logs go to a file, never to stdout/stderr (that would corrupt the TUI).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Initialises file logging in `dir/pgtui.log`. `PGTUI_LOG` (env-filter
/// syntax) overrides `level`. Keep the guard alive until exit.
pub fn init(dir: &Path, level: &str) -> Result<(WorkerGuard, PathBuf)> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let appender = tracing_appender::rolling::never(dir, "pgtui.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("PGTUI_LOG")
        .or_else(|_| EnvFilter::try_new(format!("warn,pgtui={level}")))
        .context("invalid log level")?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .init();
    Ok((guard, dir.join("pgtui.log")))
}
