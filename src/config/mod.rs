//! Configuration: `~/.config/pgtui/config.toml`.
//!
//! The file is parsed with serde, then validated. Validation never aborts on
//! soft problems (plain-text password, several password sources, ...): those
//! are collected as warnings and surfaced in the UI at startup.

mod example;
pub mod password;
pub mod pgpass;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ratatui::style::Color;
use serde::Deserialize;

pub use example::EXAMPLE_CONFIG;

/// Errors that make a configuration unusable.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Root of `config.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub editor: EditorConfig,
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
    /// `[keymap.<context>] <action> = "key" | ["key", ...]`
    #[serde(default)]
    pub keymap: BTreeMap<String, BTreeMap<String, KeyList>>,
    /// `[theme] <color name> = "red" | "#rrggbb"`
    #[serde(default)]
    pub theme: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct General {
    /// Rows fetched per page in the Data tab.
    pub page_size: usize,
    /// Connection preselected in the picker.
    pub default_connection: Option<String>,
    /// Keymap preset; `[keymap]` overrides are applied on top of it.
    pub keymap: String,
    /// `dark` or `light`.
    pub theme: String,
    /// Mouse capture (disables the terminal's native text selection).
    pub mouse: bool,
    /// `statement_timeout` applied to every session, in seconds (0 = none).
    pub statement_timeout_secs: u64,
    /// Show `pg_catalog`, `information_schema`, ... in the tree.
    pub show_system_schemas: bool,
    /// Log level for `~/.local/state/pgtui/pgtui.log` (overridden by `PGTUI_LOG`).
    pub log_level: String,
    /// `auto`, `system` or `osc52`.
    pub clipboard: ClipboardMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardMode {
    /// OSC 52 over SSH, else the system clipboard with OSC 52 as fallback.
    #[default]
    Auto,
    System,
    Osc52,
}

impl Default for General {
    fn default() -> Self {
        Self {
            page_size: 200,
            default_connection: None,
            keymap: "default".into(),
            theme: "dark".into(),
            mouse: false,
            statement_timeout_secs: 0,
            show_system_schemas: false,
            log_level: "info".into(),
            clipboard: ClipboardMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    /// Always in insert mode, Ctrl/Alt shortcuts.
    #[default]
    Classic,
    /// Modal editing (normal / insert / visual).
    Vim,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EditorConfig {
    pub mode: EditorMode,
    pub tab_width: u8,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            mode: EditorMode::Classic,
            tab_width: 4,
        }
    }
}

/// TLS mode. Same semantics as libpq: `require` encrypts without checking the
/// certificate, `verify-ca` checks the chain, `verify-full` also the host name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        }
    }
}

impl FromStr for SslMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "disable" => SslMode::Disable,
            "prefer" | "allow" => SslMode::Prefer,
            "require" => SslMode::Require,
            "verify-ca" => SslMode::VerifyCa,
            "verify-full" => SslMode::VerifyFull,
            other => return Err(format!("unknown sslmode `{other}`")),
        })
    }
}

/// One `[[connections]]` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub name: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Defaults to `$PGUSER`, then `$USER`.
    #[serde(default)]
    pub user: Option<String>,
    /// Defaults to the user name.
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default)]
    pub password_cmd: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub sslmode: SslMode,
    /// PEM file with the CA(s) to trust for `verify-ca` / `verify-full`.
    #[serde(default)]
    pub sslrootcert: Option<PathBuf>,
    #[serde(default)]
    pub read_only: bool,
    /// Status bar colour (`"red"`, `"#ff8800"`, ...).
    #[serde(default)]
    pub color: Option<String>,
    /// Production connection: permanent banner and explicit confirmation
    /// before any commit. Defaults to `true` when `color` is red.
    #[serde(default)]
    pub production: Option<bool>,
    /// Overrides `general.statement_timeout_secs`.
    #[serde(default)]
    pub statement_timeout_secs: Option<u64>,
}

fn default_host() -> String {
    "localhost".into()
}

fn default_port() -> u16 {
    5432
}

impl ConnectionConfig {
    /// A connection with defaults for everything but the name.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host: default_host(),
            port: default_port(),
            user: None,
            database: None,
            password_env: None,
            password_cmd: None,
            password: None,
            sslmode: SslMode::default(),
            sslrootcert: None,
            read_only: false,
            color: None,
            production: None,
            statement_timeout_secs: None,
        }
    }

    pub fn effective_user(&self) -> String {
        self.user
            .clone()
            .or_else(|| std::env::var("PGUSER").ok())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "postgres".into())
    }

    pub fn effective_database(&self) -> String {
        self.database
            .clone()
            .unwrap_or_else(|| self.effective_user())
    }

    pub fn parsed_color(&self) -> Option<Color> {
        self.color.as_deref().and_then(|c| parse_color(c).ok())
    }

    pub fn is_production(&self) -> bool {
        self.production
            .unwrap_or_else(|| matches!(self.parsed_color(), Some(Color::Red | Color::LightRed)))
    }

    pub fn statement_timeout(&self, general: &General) -> u64 {
        self.statement_timeout_secs
            .unwrap_or(general.statement_timeout_secs)
    }

    /// `user@host:port/db`, for display.
    pub fn target(&self) -> String {
        format!(
            "{}@{}:{}/{}",
            self.effective_user(),
            self.host,
            self.port,
            self.effective_database()
        )
    }
}

/// `"x"` or `["x", "y"]` in the `[keymap]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum KeyList {
    One(String),
    Many(Vec<String>),
}

impl KeyList {
    pub fn keys(&self) -> Vec<&str> {
        match self {
            KeyList::One(k) => vec![k.as_str()],
            KeyList::Many(ks) => ks.iter().map(String::as_str).collect(),
        }
    }
}

/// Parses a colour name (`red`, `light-blue`, `orange`), `#rrggbb` or an
/// ANSI index (`208`).
pub fn parse_color(s: &str) -> Result<Color, String> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "orange" => return Ok(Color::Indexed(208)),
        "purple" => return Ok(Color::Magenta),
        _ => {}
    }
    Color::from_str(s).map_err(|_| format!("unknown color `{s}`"))
}

/// Standard locations (XDG on Linux).
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> anyhow::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "pgtui")
            .ok_or_else(|| anyhow::anyhow!("cannot determine the home directory"))?;
        let data_dir = dirs.data_local_dir().to_path_buf();
        let state_dir = dirs
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.clone());
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            data_dir,
            state_dir,
        })
    }
}

impl Config {
    /// Parses and validates TOML. Returns the config and soft warnings.
    pub fn parse(text: &str, path: &Path) -> Result<(Self, Vec<String>), ConfigError> {
        let config: Config = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(e),
        })?;
        let warnings = config.validate()?;
        Ok((config, warnings))
    }

    /// Loads the file, creating it from [`EXAMPLE_CONFIG`] if missing.
    /// The boolean is `true` when the file has just been created.
    pub fn load_or_create(path: &Path) -> Result<(Self, Vec<String>, bool), ConfigError> {
        let mut created = false;
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|source| ConfigError::Write {
                    path: dir.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(path, EXAMPLE_CONFIG).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
            created = true;
        }
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let (config, warnings) = Self::parse(&text, path)?;
        Ok((config, warnings, created))
    }

    pub fn connection(&self, name: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
    }

    fn validate(&self) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();
        let g = &self.general;
        if g.page_size == 0 {
            return Err(ConfigError::Invalid("general.page_size must be > 0".into()));
        }
        if g.keymap != "default" {
            return Err(ConfigError::Invalid(format!(
                "general.keymap: unknown preset `{}` (only `default` exists; customise keys in [keymap])",
                g.keymap
            )));
        }
        if !matches!(g.theme.as_str(), "dark" | "light") {
            return Err(ConfigError::Invalid(format!(
                "general.theme must be `dark` or `light`, got `{}`",
                g.theme
            )));
        }
        if self.editor.tab_width == 0 || self.editor.tab_width > 16 {
            return Err(ConfigError::Invalid(
                "editor.tab_width must be in 1..=16".into(),
            ));
        }

        let mut names = HashSet::new();
        for c in &self.connections {
            if c.name.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "a connection has an empty name".into(),
                ));
            }
            if !names.insert(c.name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate connection name `{}`",
                    c.name
                )));
            }
            if let Some(color) = &c.color {
                parse_color(color)
                    .map_err(|e| ConfigError::Invalid(format!("connection `{}`: {e}", c.name)))?;
            }
            let sources = [
                c.password.is_some(),
                c.password_env.is_some(),
                c.password_cmd.is_some(),
            ]
            .iter()
            .filter(|b| **b)
            .count();
            if sources > 1 {
                warnings.push(format!(
                    "connection `{}`: several password sources, priority is password_cmd > password_env > password",
                    c.name
                ));
            }
            if c.password.is_some() {
                warnings.push(format!(
                    "connection `{}`: plain-text `password` in config.toml is discouraged, prefer password_env or password_cmd",
                    c.name
                ));
            }
            if c.sslrootcert.is_some()
                && !matches!(c.sslmode, SslMode::VerifyCa | SslMode::VerifyFull)
            {
                warnings.push(format!(
                    "connection `{}`: sslrootcert is only used with sslmode verify-ca / verify-full",
                    c.name
                ));
            }
        }
        if let Some(def) = &g.default_connection
            && !names.contains(def.as_str())
        {
            warnings.push(format!(
                "general.default_connection `{def}` does not match any connection"
            ));
        }
        Ok(warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(text: &str) -> Result<(Config, Vec<String>), ConfigError> {
        Config::parse(text, Path::new("test.toml"))
    }

    #[test]
    fn example_config_is_valid() {
        let (config, warnings) = parse(EXAMPLE_CONFIG).expect("example must parse");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(config.general.page_size, 200);
        assert_eq!(config.general.default_connection.as_deref(), Some("local"));
        assert!(config.connection("local").is_some());
    }

    #[test]
    fn empty_file_gives_defaults() {
        let (config, warnings) = parse("").unwrap();
        assert!(warnings.is_empty());
        assert_eq!(config.general.page_size, 200);
        assert_eq!(config.general.theme, "dark");
        assert_eq!(config.editor.mode, EditorMode::Classic);
        assert!(config.connections.is_empty());
    }

    #[test]
    fn full_connection() {
        let (config, _) = parse(
            r##"
            [general]
            page_size = 50
            default_connection = "prod"
            statement_timeout_secs = 30

            [editor]
            mode = "vim"

            [[connections]]
            name = "prod"
            host = "db.example.com"
            port = 6432
            user = "app"
            database = "shop"
            password_env = "PGPASSWORD_PROD"
            sslmode = "verify-full"
            read_only = true
            color = "red"
            statement_timeout_secs = 5
            "##,
        )
        .unwrap();
        let c = config.connection("prod").unwrap();
        assert_eq!(c.host, "db.example.com");
        assert_eq!(c.port, 6432);
        assert_eq!(c.sslmode, SslMode::VerifyFull);
        assert!(c.read_only);
        assert!(c.is_production(), "red implies production");
        assert_eq!(c.statement_timeout(&config.general), 5);
        assert_eq!(c.target(), "app@db.example.com:6432/shop");
        assert_eq!(config.editor.mode, EditorMode::Vim);
    }

    #[test]
    fn connection_defaults() {
        let (config, _) = parse("[[connections]]\nname = \"x\"\nuser = \"bob\"").unwrap();
        let c = &config.connections[0];
        assert_eq!(c.host, "localhost");
        assert_eq!(c.port, 5432);
        assert_eq!(c.sslmode, SslMode::Prefer);
        assert_eq!(c.effective_database(), "bob");
        assert!(!c.is_production());
        assert!(!c.read_only);
    }

    #[test]
    fn production_flag_overrides_color() {
        let (config, _) = parse(
            "[[connections]]\nname = \"a\"\ncolor = \"red\"\nproduction = false\n\
             [[connections]]\nname = \"b\"\ncolor = \"#00ff00\"\nproduction = true",
        )
        .unwrap();
        assert!(!config.connections[0].is_production());
        assert!(config.connections[1].is_production());
    }

    #[test]
    fn plain_password_warns() {
        let (_, warnings) =
            parse("[[connections]]\nname = \"a\"\npassword = \"x\"\npassword_env = \"Y\"").unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("plain-text")));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("several password sources"))
        );
    }

    #[test]
    fn rejects_unknown_fields_and_values() {
        assert!(parse("[general]\npage_sise = 10").is_err());
        assert!(parse("[[connections]]\nname = \"a\"\nsslmode = \"maybe\"").is_err());
        assert!(parse("[[connections]]\nname = \"a\"\ncolor = \"blurple\"").is_err());
        assert!(parse("[general]\npage_size = 0").is_err());
        assert!(parse("[general]\ntheme = \"pink\"").is_err());
        assert!(parse("[general]\nkeymap = \"emacs\"").is_err());
    }

    #[test]
    fn rejects_duplicate_names() {
        let err =
            parse("[[connections]]\nname = \"a\"\n[[connections]]\nname = \"a\"").unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn unknown_default_connection_warns() {
        let (_, warnings) = parse("[general]\ndefault_connection = \"nope\"").unwrap();
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn keymap_section() {
        let (config, _) =
            parse("[keymap.global]\nquit = \"ctrl+q\"\n[keymap.tree]\nexpand = [\"l\", \"right\"]")
                .unwrap();
        assert_eq!(config.keymap["global"]["quit"].keys(), vec!["ctrl+q"]);
        assert_eq!(config.keymap["tree"]["expand"].keys(), vec!["l", "right"]);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("red"), Ok(Color::Red));
        assert_eq!(parse_color("#ff8800"), Ok(Color::Rgb(0xff, 0x88, 0x00)));
        assert_eq!(parse_color("orange"), Ok(Color::Indexed(208)));
        assert!(parse_color("nope").is_err());
    }

    #[test]
    fn load_or_create_writes_example() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let (_, _, created) = Config::load_or_create(&path).unwrap();
        assert!(created);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXAMPLE_CONFIG);
        let (_, _, created) = Config::load_or_create(&path).unwrap();
        assert!(!created);
    }
}
