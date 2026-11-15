//! The single event loop: terminal events, DB events, ticks and signals all
//! become [`Msg`]s handled by [`App::update`]; the resulting commands go to
//! the DB worker.

use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Msg};
use crate::db::worker;
use crate::terminal::Tui;
use crate::ui;

const TICK: Duration = Duration::from_millis(100);

pub async fn run(terminal: &mut Tui, mut app: App) -> Result<()> {
    let (event_tx, mut db_events) = unbounded_channel();
    let commands = worker::spawn(event_tx);
    for cmd in app.init() {
        commands.send(cmd)?;
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
        for cmd in app.update(msg) {
            commands.send(cmd)?;
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}
