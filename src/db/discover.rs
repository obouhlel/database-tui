//! Discovery of PostgreSQL servers on the local machine.
//!
//! Each candidate (TCP on 127.0.0.1 / ::1, Unix sockets in the usual
//! directories) is opened and sent an `SSLRequest`: a PostgreSQL server
//! answers with a single `S` or `N` byte, which identifies it without
//! authenticating or logging in.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Directories where PostgreSQL puts its Unix socket.
pub const SOCKET_DIRS: &[&str] = &["/var/run/postgresql", "/run/postgresql", "/tmp"];

const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// `SSLRequest`: length 8, code 80877103.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredServer {
    /// `127.0.0.1`, `::1` or a socket directory.
    pub host: String,
    pub port: u16,
    /// The server accepts TLS (`S` answer).
    pub tls: bool,
}

impl DiscoveredServer {
    pub fn is_socket(&self) -> bool {
        self.host.starts_with('/')
    }

    /// `localhost:5432` or `socket /var/run/postgresql:5432`.
    pub fn label(&self) -> String {
        match self.host.as_str() {
            h if self.is_socket() => format!("socket {h}:{}", self.port),
            "::1" => format!("[::1]:{}", self.port),
            h => format!("{h}:{}", self.port),
        }
    }
}

/// Probes every candidate concurrently. IPv6 loopback is dropped when IPv4
/// answered on the same port (same server, almost always).
pub async fn scan(ports: &[u16]) -> Vec<DiscoveredServer> {
    let mut probes = Vec::new();
    for &port in ports {
        for host in ["127.0.0.1", "::1"] {
            probes.push(tokio::spawn(probe_tcp(host, port)));
        }
        for dir in SOCKET_DIRS {
            probes.push(tokio::spawn(probe_socket(dir, port)));
        }
    }
    let mut found = Vec::new();
    for p in probes {
        if let Ok(Some(s)) = p.await {
            found.push(s);
        }
    }
    let v4_ports: Vec<u16> = found
        .iter()
        .filter(|s| s.host == "127.0.0.1")
        .map(|s| s.port)
        .collect();
    found.retain(|s| !(s.host == "::1" && v4_ports.contains(&s.port)));
    // Several directories can be links to the same one.
    found.dedup_by(|a, b| {
        a.is_socket() && b.is_socket() && a.port == b.port && same_dir(&a.host, &b.host)
    });
    found
}

fn same_dir(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

async fn probe_tcp(host: &'static str, port: u16) -> Option<DiscoveredServer> {
    let stream = tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect((host, port)))
        .await
        .ok()?
        .ok()?;
    let tls = handshake(stream).await?;
    Some(DiscoveredServer {
        host: host.to_string(),
        port,
        tls,
    })
}

#[cfg(unix)]
async fn probe_socket(dir: &'static str, port: u16) -> Option<DiscoveredServer> {
    let path = Path::new(dir).join(format!(".s.PGSQL.{port}"));
    if !path.exists() {
        return None;
    }
    let stream = tokio::time::timeout(PROBE_TIMEOUT, tokio::net::UnixStream::connect(&path))
        .await
        .ok()?
        .ok()?;
    let tls = handshake(stream).await?;
    Some(DiscoveredServer {
        host: dir.to_string(),
        port,
        tls,
    })
}

#[cfg(not(unix))]
async fn probe_socket(_dir: &'static str, _port: u16) -> Option<DiscoveredServer> {
    None
}

/// Sends `SSLRequest`; `Some(tls)` if the peer speaks PostgreSQL.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> Option<bool> {
    let exchange = async {
        stream.write_all(&SSL_REQUEST).await.ok()?;
        let mut answer = [0u8; 1];
        stream.read_exact(&mut answer).await.ok()?;
        match answer[0] {
            b'S' => Some(true),
            b'N' => Some(false),
            _ => None,
        }
    };
    tokio::time::timeout(PROBE_TIMEOUT, exchange).await.ok()?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    /// A fake server answering `answer` to the first 8 bytes.
    async fn fake(answer: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 8];
                if s.read_exact(&mut buf).await.is_ok() && buf == SSL_REQUEST {
                    let _ = s.write_all(answer).await;
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn recognises_postgres_answers() {
        let with_tls = fake(b"S").await;
        let without = fake(b"N").await;
        let http = fake(b"HTTP/1.1 400").await;
        let found = scan(&[with_tls, without, http]).await;
        let tcp: Vec<_> = found.iter().filter(|s| !s.is_socket()).collect();
        assert!(
            tcp.iter()
                .any(|s| s.port == with_tls && s.tls && s.host == "127.0.0.1")
        );
        assert!(tcp.iter().any(|s| s.port == without && !s.tls));
        assert!(!tcp.iter().any(|s| s.port == http), "not PostgreSQL");
    }

    #[tokio::test]
    async fn closed_port_is_ignored() {
        // Bind then drop: the port is (very likely) closed.
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(probe_tcp("127.0.0.1", port).await.is_none());
    }

    #[test]
    fn labels() {
        let s = |host: &str| DiscoveredServer {
            host: host.into(),
            port: 5432,
            tls: false,
        };
        assert_eq!(s("127.0.0.1").label(), "127.0.0.1:5432");
        assert_eq!(s("::1").label(), "[::1]:5432");
        assert_eq!(
            s("/var/run/postgresql").label(),
            "socket /var/run/postgresql:5432"
        );
    }
}
