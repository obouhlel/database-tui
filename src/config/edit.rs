//! Editing `config.toml` by hand from the application: the block added for
//! a new connection, and where to place the editor's cursor.

use super::{ConfigError, ConnectionConfig};

/// Name of the connection added by the template (made unique).
const NEW_NAME: &str = "nouvelle";

/// The template block for a new connection, named `name`. Every line the
/// user may need is there; the optional ones are commented out.
pub fn template(name: &str) -> String {
    format!(
        r#"
# Nouvelle connexion : complétez, enregistrez et quittez l'éditeur.
# pgtui la teste aussitôt. Laissée telle quelle, elle est retirée.
[[connections]]
name = "{name}"
host = "localhost"
port = 5432
user = "postgres"
database = "postgres"
# Mot de passe (un seul) : commande, variable d'environnement, ou ~/.pgpass.
# password_cmd = "pass show postgres/{name}"
# password_env = "PGPASSWORD_{upper}"
# sslmode = "prefer"           # disable | prefer | require | verify-ca | verify-full
# tunnel_cmd = "ssh -N -L {{port}}:db.interne:5432 bastion"
# read_only = true
# color = "red"                # rouge : connexion de production (confirmation)
"#,
        upper = name.to_ascii_uppercase().replace('-', "_"),
    )
}

/// `content` with the template appended, and the 1-based line of its
/// `name = ...` (where the editor opens).
pub fn with_template(content: &str, existing: &[ConnectionConfig]) -> (String, usize) {
    let mut name = NEW_NAME.to_string();
    let mut n = 2;
    while existing.iter().any(|c| c.name == name) {
        name = format!("{NEW_NAME}-{n}");
        n += 1;
    }
    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&template(&name));
    let line = connection_line(&out, &name).unwrap_or_else(|| out.lines().count());
    (out, line)
}

/// 1-based line of `name = "<name>"` inside a `[[connections]]` block.
pub fn connection_line(content: &str, name: &str) -> Option<usize> {
    let mut in_connection = false;
    for (i, line) in content.lines().enumerate() {
        let t = line.trim();
        if t.starts_with('[') {
            in_connection = t == "[[connections]]";
            continue;
        }
        if !in_connection {
            continue;
        }
        let Some((key, value)) = t.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        let value = value.split('#').next().unwrap_or("").trim();
        let parsed: Option<String> = toml::from_str::<toml::Value>(&format!("v = {value}"))
            .ok()
            .and_then(|v| v.get("v").and_then(|v| v.as_str().map(str::to_string)));
        if parsed.as_deref() == Some(name) {
            return Some(i + 1);
        }
    }
    None
}

/// 1-based line of a syntax error, to reopen the editor on it.
pub fn error_line(err: &ConfigError, content: &str) -> Option<usize> {
    match err {
        ConfigError::Parse { source, .. } => {
            let start = source.span()?.start.min(content.len());
            Some(content[..start].matches('\n').count() + 1)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::Path;

    const BASE: &str = "[general]\npage_size = 50\n\n[[connections]]\nname = \"nouvelle\"\n\n[[connections]]\nname = 'prod' # la prod\nport = 5433\n";

    #[test]
    fn template_parses_with_a_unique_name() {
        let (existing, _) = Config::parse(BASE, Path::new("c.toml")).unwrap();
        let (text, line) = with_template(BASE, &existing.connections);
        let (config, _) = Config::parse(&text, Path::new("c.toml")).unwrap();
        let added = config.connections.last().unwrap();
        assert_eq!(added.name, "nouvelle-2");
        assert_eq!(added.port, 5432);
        assert_eq!(text.lines().nth(line - 1), Some("name = \"nouvelle-2\""));
        assert!(text.starts_with(BASE), "the rest is untouched");
    }

    #[test]
    fn finds_the_line_of_a_connection() {
        assert_eq!(connection_line(BASE, "nouvelle"), Some(5));
        assert_eq!(connection_line(BASE, "prod"), Some(8));
        assert_eq!(connection_line(BASE, "absente"), None);
        // `name` outside [[connections]] does not count.
        assert_eq!(
            connection_line("[general]\nname = \"prod\"\n", "prod"),
            None
        );
    }

    #[test]
    fn line_of_a_syntax_error() {
        let text = "[general]\npage_size = 50\n\n[[connections]]\nname = \"x\nport = 1\n";
        let err = Config::parse(text, Path::new("c.toml")).unwrap_err();
        assert_eq!(error_line(&err, text), Some(5));
    }
}
