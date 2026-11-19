//! `pgtui --check-connections`: tests every connection of `config.toml` (or
//! the ones named) in parallel and prints one line per connection. The exit
//! code is non-zero when one of them fails, for scripts and cron jobs.

use std::io::{self, IsTerminal, Write};

use anyhow::{Result, bail};

use crate::config::ConnectionConfig;
use crate::db::probe::{self, ProbeOk};

/// The connections to test: all of them, or the ones named (in that order).
pub fn select<'a>(
    all: &'a [ConnectionConfig],
    names: &[String],
) -> Result<Vec<&'a ConnectionConfig>> {
    if names.is_empty() {
        if all.is_empty() {
            bail!("aucune connexion dans la configuration");
        }
        return Ok(all.iter().collect());
    }
    let mut out = Vec::new();
    for n in names {
        match all.iter().find(|c| &c.name == n) {
            Some(c) => out.push(c),
            None => {
                let known: Vec<_> = all.iter().map(|c| c.name.as_str()).collect();
                bail!(
                    "connexion inconnue `{n}` (disponibles : {})",
                    known.join(", ")
                );
            }
        }
    }
    Ok(out)
}

/// Tests the connections and prints the report. Returns `true` when all
/// of them answered.
pub async fn run(conns: &[&ConnectionConfig]) -> Result<bool> {
    let results = futures::future::join_all(conns.iter().map(|c| probe::probe(c, None))).await;
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = io::stdout().lock();
    for line in report(conns, &results, color) {
        writeln!(out, "{line}")?;
    }
    Ok(results.iter().all(Result::is_ok))
}

/// One line per connection, then a summary.
pub fn report(
    conns: &[&ConnectionConfig],
    results: &[Result<ProbeOk, String>],
    color: bool,
) -> Vec<String> {
    let paint = |code: &str, s: &str| {
        if color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    };
    let name_w = conns
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0);
    let targets: Vec<String> = conns.iter().map(|c| probe::describe(c)).collect();
    let target_w = targets.iter().map(|t| t.chars().count()).max().unwrap_or(0);
    let mut lines = Vec::new();
    for ((c, target), r) in conns.iter().zip(&targets).zip(results) {
        let (status, detail) = match r {
            Ok(ok) => (
                paint("32;1", "OK    "),
                format!(
                    "PostgreSQL {}{} · {} ms",
                    ok.version,
                    if ok.tls { " · TLS" } else { "" },
                    ok.elapsed.as_millis()
                ),
            ),
            Err(e) => (
                paint("31;1", "ÉCHEC "),
                paint("31", e.lines().next().unwrap_or("")),
            ),
        };
        lines.push(format!(
            "{status} {:name_w$}  {:target_w$}  {detail}",
            c.name, target
        ));
    }
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let summary = format!("{ok}/{} connexion(s) OK", results.len());
    lines.push(String::new());
    lines.push(if ok == results.len() {
        paint("32;1", &summary)
    } else {
        paint("31;1", &summary)
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn conn(name: &str) -> ConnectionConfig {
        toml::from_str(&format!(
            "name = \"{name}\"\nuser = \"app\"\ndatabase = \"main\""
        ))
        .unwrap()
    }

    #[test]
    fn selects_all_or_the_named_ones() {
        let all = vec![conn("a"), conn("b"), conn("c")];
        assert_eq!(select(&all, &[]).unwrap().len(), 3);
        let some = select(&all, &["c".into(), "a".into()]).unwrap();
        assert_eq!(
            some.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["c", "a"]
        );
        let e = select(&all, &["x".into()]).unwrap_err().to_string();
        assert!(e.contains("`x`") && e.contains("a, b, c"), "{e}");
        assert!(select(&[], &[]).is_err());
    }

    #[test]
    fn report_lines_are_aligned() {
        let a = conn("prod");
        let b = conn("recette-longue");
        let ok = ProbeOk {
            version: "16.4".into(),
            user: "app".into(),
            database: "main".into(),
            tls: true,
            elapsed: Duration::from_millis(42),
        };
        let lines = report(
            &[&a, &b],
            &[Ok(ok), Err("connection refused\nmore".into())],
            false,
        );
        assert_eq!(
            lines[0],
            "OK     prod            app@localhost:5432/main  PostgreSQL 16.4 · TLS · 42 ms"
        );
        assert_eq!(
            lines[1],
            "ÉCHEC  recette-longue  app@localhost:5432/main  connection refused"
        );
        assert_eq!(lines[3], "1/2 connexion(s) OK");
    }
}
