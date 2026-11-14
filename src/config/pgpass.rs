//! Minimal `~/.pgpass` support (same format and lookup rules as libpq).
//!
//! Each line is `hostname:port:database:username:password`; `*` matches
//! anything, `\:` and `\\` are escapes, `#` starts a comment line.

use std::path::PathBuf;

/// Location of the password file: `$PGPASSFILE` or `~/.pgpass`.
pub fn default_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PGPASSFILE") {
        return Some(PathBuf::from(p));
    }
    directories::BaseDirs::new().map(|d| d.home_dir().join(".pgpass"))
}

/// Looks up a password in the file content. The first matching line wins.
pub fn lookup(content: &str, host: &str, port: u16, database: &str, user: &str) -> Option<String> {
    let port = port.to_string();
    // libpq treats unix socket hosts as "localhost" for matching.
    let host = if host.starts_with('/') {
        "localhost"
    } else {
        host
    };
    content
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .filter_map(split_line)
        .find(|f| {
            matches(&f[0], host)
                && matches(&f[1], &port)
                && matches(&f[2], database)
                && matches(&f[3], user)
        })
        .map(|mut f| f.swap_remove(4))
}

/// Reads the default file and looks up a password (`None` if missing).
pub fn lookup_default(host: &str, port: u16, database: &str, user: &str) -> Option<String> {
    let path = default_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    lookup(&content, host, port, database, user)
}

fn matches(pattern: &str, value: &str) -> bool {
    pattern == "*" || pattern == value
}

fn split_line(line: &str) -> Option<Vec<String>> {
    let mut fields = Vec::with_capacity(5);
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            // The password (5th field) may contain unescaped colons.
            ':' if fields.len() < 4 => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    (fields.len() == 5).then_some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r"# comment
db.example.com:5432:shop:app:s3cret
*:5433:demo:pgtui:demo\:pass
localhost:*:*:postgres:local
*:*:*:*:fallback
";

    #[test]
    fn exact_match() {
        assert_eq!(
            lookup(FILE, "db.example.com", 5432, "shop", "app").as_deref(),
            Some("s3cret")
        );
    }

    #[test]
    fn wildcards_and_escapes() {
        assert_eq!(
            lookup(FILE, "anything", 5433, "demo", "pgtui").as_deref(),
            Some("demo:pass")
        );
        assert_eq!(
            lookup(FILE, "localhost", 6000, "x", "postgres").as_deref(),
            Some("local")
        );
    }

    #[test]
    fn unix_socket_counts_as_localhost() {
        assert_eq!(
            lookup(FILE, "/var/run/postgresql", 1, "x", "postgres").as_deref(),
            Some("local")
        );
    }

    #[test]
    fn first_match_wins_and_fallback() {
        assert_eq!(
            lookup(FILE, "other", 1, "x", "y").as_deref(),
            Some("fallback")
        );
        assert_eq!(lookup("a:1:b:c:d", "z", 1, "b", "c"), None);
    }

    #[test]
    fn malformed_lines_are_ignored() {
        assert_eq!(
            lookup("only:three:fields\n*:*:*:*:ok", "h", 1, "d", "u").as_deref(),
            Some("ok")
        );
    }
}
