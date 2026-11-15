//! The single event loop: terminal events, DB events, ticks and signals all
//! become [`Msg`]s handled by [`App::update`]; the resulting commands go to
//! the DB worker.

use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Effect, Level, Msg};
use crate::clipboard::Clipboard;
use crate::db::worker;
use crate::terminal::Tui;
use crate::ui;

const TICK: Duration = Duration::from_millis(100);

pub async fn run(terminal: &mut Tui, mut app: App) -> Result<()> {
    let (event_tx, mut db_events) = unbounded_channel();
    let commands = worker::spawn(event_tx);
    let mut clipboard = Clipboard::new(app.config.general.clipboard);
    for effect in app.init() {
        run_effect(effect, &commands, &mut clipboard, &mut app)?;
    }

    let mut input = crossterm::event::EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    loop {
        if app.take_dirty() {
            terminal.draw(|f| ui::draw(f, &mut app))?;
        }
        let msg = tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(ev)) => match Msg::from_terminal(ev) {
                    Some(m) => m,
                    None => continue,
                },
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(ev) = db_events.recv() => Msg::Db(ev),
            _ = tick.tick() => Msg::Tick,
            _ = sigterm.recv() => Msg::Terminate,
            _ = sighup.recv() => Msg::Terminate,
        };
        for effect in app.update(msg) {
            run_effect(effect, &commands, &mut clipboard, &mut app)?;
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn run_effect(
    effect: Effect,
    commands: &tokio::sync::mpsc::UnboundedSender<worker::DbCommand>,
    clipboard: &mut Clipboard,
    app: &mut App,
) -> Result<()> {
    match effect {
        Effect::Db(cmd) => commands.send(cmd)?,
        Effect::SaveConnection(conn) => {
            let msg = match app.config_path.clone() {
                Some(path) => match crate::config::append_connection(&path, &conn) {
                    Ok(()) => Msg::Notify(
                        Level::Success,
                        format!("« {} » ajouté à {}", conn.name, path.display()),
                    ),
                    Err(e) => Msg::Notify(Level::Error, format!("Enregistrement impossible : {e}")),
                },
                None => Msg::Notify(Level::Error, "Fichier de configuration inconnu".into()),
            };
            let _ = app.update(msg);
        }
        Effect::DeleteConnection(name) => {
            let msg = match app.config_path.clone() {
                Some(path) => match crate::config::remove_connection(&path, &name) {
                    Ok(()) => Msg::Notify(
                        Level::Success,
                        format!("« {name} » supprimé de {}", path.display()),
                    ),
                    Err(e) => Msg::Notify(Level::Error, format!("Suppression impossible : {e}")),
                },
                None => Msg::Notify(Level::Error, "Fichier de configuration inconnu".into()),
            };
            let _ = app.update(msg);
        }
        Effect::WriteFile { path, text, done } => {
            let msg = match std::fs::write(&path, text) {
                Ok(()) => Msg::Notify(Level::Success, format!("{done} : {}", path.display())),
                Err(e) => Msg::Notify(
                    Level::Error,
                    format!("Export impossible : {} : {e}", path.display()),
                ),
            };
            let _ = app.update(msg);
        }
        Effect::AppendHistory(entry) => {
            if let Some(dir) = &app.data_dir {
                let path = crate::storage::history_path(dir);
                if let Err(e) = crate::storage::append_history(&path, &entry) {
                    tracing::warn!("history not saved: {e:#}");
                }
            }
        }
        Effect::SaveQuery { name, text } => {
            let msg = match &app.data_dir {
                Some(dir) => match crate::storage::save_query(
                    &crate::storage::queries_dir(dir),
                    &name,
                    &text,
                ) {
                    Ok(path) => Msg::Notify(
                        Level::Success,
                        format!("Requête enregistrée : {}", path.display()),
                    ),
                    Err(e) => {
                        Msg::Notify(Level::Error, format!("Enregistrement impossible : {e:#}"))
                    }
                },
                None => Msg::Notify(Level::Error, "Répertoire de données inconnu".into()),
            };
            let _ = app.update(msg);
        }
        Effect::LoadQuery(name) => {
            let msg = match &app.data_dir {
                Some(dir) => {
                    match crate::storage::load_query(&crate::storage::queries_dir(dir), &name) {
                        Ok(text) => Msg::QueryLoaded { name, text },
                        Err(e) => {
                            Msg::Notify(Level::Error, format!("Ouverture impossible : {e:#}"))
                        }
                    }
                }
                None => Msg::Notify(Level::Error, "Répertoire de données inconnu".into()),
            };
            let _ = app.update(msg);
        }
        Effect::ListQueries => {
            let names = app
                .data_dir
                .as_ref()
                .map(|d| crate::storage::list_queries(&crate::storage::queries_dir(d)))
                .unwrap_or_default();
            let _ = app.update(Msg::QueriesListed(names));
        }
        Effect::Copy(text) => match clipboard.copy(&text) {
            Ok(method) => tracing::debug!(bytes = text.len(), method, "copied"),
            Err(e) => {
                // Follow-up effects of a notification are always empty.
                let _ = app.update(Msg::Notify(
                    Level::Error,
                    format!("Copie impossible : {e:#}"),
                ));
            }
        },
    }
    Ok(())
}
