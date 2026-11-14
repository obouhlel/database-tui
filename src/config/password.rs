//! Password resolution for a connection.
//!
//! Order: `password_cmd` > `password_env` > `password` > `~/.pgpass`.
//! When nothing is found the connection is attempted without a password
//! (trust / peer auth); the UI prompts if the server asks for one.

use anyhow::{Context, Result, bail};

use super::{ConnectionConfig, pgpass};

/// Where a password came from (for logs, never the value itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordSource {
    Command,
    Env,
    Plain,
    PgPass,
    Prompt,
}

pub async fn resolve(conn: &ConnectionConfig) -> Result<Option<(String, PasswordSource)>> {
    if let Some(cmd) = &conn.password_cmd {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .with_context(|| format!("cannot run password_cmd `{cmd}`"))?;
        if !output.status.success() {
            bail!(
                "password_cmd `{cmd}` failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout =
            String::from_utf8(output.stdout).context("password_cmd output is not UTF-8")?;
        // Like `pass`, the password is the first line.
        let password = stdout.lines().next().unwrap_or_default().to_string();
        return Ok(Some((password, PasswordSource::Command)));
    }
    if let Some(var) = &conn.password_env {
        match std::env::var(var) {
            Ok(v) => return Ok(Some((v, PasswordSource::Env))),
            Err(_) => {
                tracing::warn!(connection = %conn.name, var, "password_env variable is not set")
            }
        }
    }
    if let Some(p) = &conn.password {
        return Ok(Some((p.clone(), PasswordSource::Plain)));
    }
    let found = pgpass::lookup_default(
        &conn.host,
        conn.port,
        &conn.effective_database(),
        &conn.effective_user(),
    );
    Ok(found.map(|p| (p, PasswordSource::PgPass)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_takes_first_line() {
        let mut c = ConnectionConfig::named("t");
        c.password_cmd = Some("printf 'abc\\nsecond line'".into());
        c.password = Some("ignored".into());
        let (p, src) = resolve(&c).await.unwrap().unwrap();
        assert_eq!(p, "abc");
        assert_eq!(src, PasswordSource::Command);
    }

    #[tokio::test]
    async fn failing_command_is_an_error() {
        let mut c = ConnectionConfig::named("t");
        c.password_cmd = Some("echo oops >&2; exit 3".into());
        let err = resolve(&c).await.unwrap_err().to_string();
        assert!(err.contains("oops"), "{err}");
    }

    #[tokio::test]
    async fn missing_env_falls_back_to_plain() {
        let mut c = ConnectionConfig::named("t");
        c.password_env = Some("PGTUI_TEST_SURELY_UNSET_VAR".into());
        c.password = Some("plain".into());
        let (p, src) = resolve(&c).await.unwrap().unwrap();
        assert_eq!((p.as_str(), src), ("plain", PasswordSource::Plain));
    }
}
