use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;

use pgtui::app::{App, Start};
use pgtui::cli::Cli;
use pgtui::config::{Config, Paths};
use pgtui::keymap::Keymap;
use pgtui::ui::theme::Theme;
use pgtui::{db, logging, run, terminal};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match real_main(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            terminal::restore();
            eprintln!("pgtui: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn real_main(cli: Cli) -> Result<()> {
    let paths = Paths::discover()?;
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| paths.config_file.clone());
    let (config, mut warnings, created) = Config::load_or_create(&config_path)?;

    let (keymap, km_warnings) = Keymap::with_overrides(&config.keymap);
    let (theme, theme_warnings) = Theme::from_config(&config.general.theme, &config.theme);
    warnings.extend(km_warnings);
    warnings.extend(theme_warnings);

    if cli.check_config {
        println!(
            "{}: OK ({} connection(s))",
            config_path.display(),
            config.connections.len()
        );
        for w in &warnings {
            println!("warning: {w}");
        }
        return Ok(());
    }

    let (_log_guard, log_file) = logging::init(&paths.state_dir, &config.general.log_level)?;
    tracing::info!(config = %config_path.display(), "pgtui {} starting", env!("CARGO_PKG_VERSION"));
    if created {
        warnings.insert(
            0,
            format!("Configuration d'exemple créée : {}", config_path.display()),
        );
    }
    for w in &warnings {
        tracing::warn!("{w}");
    }

    // Extra picker entries and the startup target.
    let mut extra = Vec::new();
    let start = if let Some(url) = &cli.url {
        let (conn, password) = db::connection_from_url("url", url)?;
        extra.push((conn.clone(), password.clone()));
        Start::Connect {
            conn: Box::new(conn),
            password,
        }
    } else if let Some(name) = &cli.connection {
        let Some(conn) = config.connection(name).cloned() else {
            let names: Vec<_> = config.connections.iter().map(|c| c.name.as_str()).collect();
            bail!(
                "unknown connection `{name}` (available: {})",
                names.join(", ")
            );
        };
        Start::Connect {
            conn: Box::new(conn),
            password: None,
        }
    } else {
        Start::Picker
    };
    if cli.url.is_none()
        && let Ok(url) = std::env::var("DATABASE_URL")
    {
        match db::connection_from_url("DATABASE_URL", &url) {
            Ok(entry) => extra.push(entry),
            Err(e) => warnings.push(format!("DATABASE_URL ignored: {e:#}")),
        }
    }

    let mouse = config.general.mouse;
    let mut app = App::new(config, keymap, theme, warnings, extra, start);
    app.config_path = Some(config_path.clone());
    app.sql.history = pgtui::storage::load_history(&pgtui::storage::history_path(&paths.data_dir));
    app.data_dir = Some(paths.data_dir.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    let result = runtime.block_on(async {
        let (mut tui, _guard) = terminal::enter(mouse)?;
        run::run(&mut tui, app).await
    });
    terminal::restore();
    if let Err(e) = &result {
        tracing::error!("fatal: {e:#}");
        eprintln!("(logs : {})", log_file.display());
    }
    // Do not wait for in-flight queries or password commands.
    runtime.shutdown_timeout(std::time::Duration::from_millis(200));
    result
}
