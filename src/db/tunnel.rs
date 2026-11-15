//! Tunnels opened by an external command before connecting (`tunnel_cmd`).
//!
//! The command template contains `{port}`: pgtui picks a free local port,
//! substitutes it, runs the command (`sh -c`, stdin closed so that it never
//! waits for a prompt), waits until the port accepts connections, then
//! connects to `127.0.0.1:{port}`. The process is killed when the tunnel is
//! dropped (disconnection, new connection, exit).

use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

pub const PORT_PLACEHOLDER: &str = "{port}";

pub struct Tunnel {
    child: Child,
    pub port: u16,
    /// The command actually run (for logs).
    pub command: String,
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel")
            .field("port", &self.port)
            .field("command", &self.command)
            .finish()
    }
}

impl Tunnel {
    /// `Some(reason)` once the tunnel process has exited.
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(format!("le tunnel s'est arrêté ({status})")),
            Ok(None) => None,
            Err(e) => Some(format!("état du tunnel inconnu : {e}")),
        }
    }
}

/// A free TCP port on the loopback interface.
fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).context("no free local port")?;
    Ok(listener.local_addr()?.port())
}

/// Runs `template` with `{port}` replaced and waits for the port to open.
pub async fn open(template: &str, timeout: Duration) -> Result<Tunnel> {
    if !template.contains(PORT_PLACEHOLDER) {
        bail!("tunnel_cmd doit contenir {PORT_PLACEHOLDER} (le port local choisi par pgtui)");
    }
    let port = free_port()?;
    let command = template.replace(PORT_PLACEHOLDER, &port.to_string());
    tracing::info!(%command, "opening tunnel");
    // `exec` makes the shell replace itself with the command, so that
    // killing the child kills ssh itself.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec {command}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot run `{command}`"))?;

    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut stderr).await;
            }
            let stderr = stderr.trim();
            return Err(anyhow!(
                "la commande du tunnel s'est arrêtée ({status}){}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(" : {stderr}")
                }
            ));
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            tracing::info!(port, elapsed = ?started.elapsed(), "tunnel ready");
            return Ok(Tunnel {
                child,
                port,
                command,
            });
        }
        if started.elapsed() > timeout {
            // Dropping the child kills it.
            bail!(
                "le tunnel n'a pas ouvert le port local {port} en {} s (commande : {command})",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake tunnel: listens on `{port}` like `ssh -L` would.
    const LISTEN: &str = "python3 -c 'import socket,time; s=socket.socket(); \
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); \
        s.bind((\"127.0.0.1\", {port})); s.listen(); time.sleep(60)'";

    #[tokio::test]
    async fn opens_waits_and_closes_on_drop() {
        let mut t = open(LISTEN, Duration::from_secs(10)).await.unwrap();
        assert!(t.command.contains(&t.port.to_string()));
        assert!(t.exited().is_none());
        let port = t.port;
        drop(t);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "process killed"
        );
    }

    #[tokio::test]
    async fn failing_command_reports_stderr() {
        let err = open(
            "echo 'Permission denied (publickey)' >&2; exit 255 # {port}",
            Duration::from_secs(5),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("Permission denied (publickey)"), "{err}");
    }

    #[tokio::test]
    async fn timeout_when_the_port_never_opens() {
        let err = open("sleep 30 # {port}", Duration::from_millis(400))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("n'a pas ouvert le port"), "{err}");
    }

    #[tokio::test]
    async fn placeholder_is_required() {
        let err = open("ssh -N -L 5433:db:5432 bastion", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("{port}"));
    }
}
