//! Listening side of the link: port discovery, `/hello`, and the WebSocket
//! upgrade. See `PROTOCOL.md` §4, §5.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Query, State, WebSocketUpgrade, ws};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use crate::link::Link;
use crate::protocol::VERSION;

/// The discovery range (§4). Twenty slots, taken first-free.
pub const PORT_RANGE: std::ops::RangeInclusive<u16> = 51820..=51839;

/// Figma documents run to 100 MB (C5); the default 64 KiB frame cap would tear
/// the connection down on any real file.
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error(
        "no free port in {}-{}: {} servers are already running, which is the limit",
        PORT_RANGE.start(), PORT_RANGE.end(), PORT_RANGE.count()
    )]
    RangeExhausted,
    #[error("bind {addr}: {source}")]
    Bind { addr: SocketAddr, source: std::io::Error },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// What a server advertises about itself on `/hello` (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub v: u32,
    /// Which host application this server drives: `figma`, `unity`, …
    pub host: String,
    pub pid: u32,
    /// Human-readable, so "which editor is this?" is answerable at a glance.
    pub label: String,
    /// Epoch milliseconds. u64 rather than u128 deliberately: #[serde(flatten)]
    /// buffers through serde_json::Value, which cannot represent u128, so a
    /// wider type here silently breaks both /hello and descriptor reading.
    pub started_at_ms: u64,
}

impl Identity {
    pub fn new(host: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            v: VERSION,
            host: host.into(),
            pid: std::process::id(),
            label: label.into(),
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

/// What `/hello` returns: the server's identity plus whether a host is already
/// attached, so a picker can show which sessions are free (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    #[serde(flatten)]
    pub identity: Identity,
    pub connected: bool,
}

/// A session descriptor, written for CLI tooling only. Nothing in the protocol
/// reads it — the host UI cannot, having no filesystem access (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFile {
    #[serde(flatten)]
    pub identity: Identity,
    pub port: u16,
}

pub struct Config {
    pub identity: Identity,
    pub bind: IpAddr,
    /// Required when `bind` is not loopback (§5); ignored otherwise.
    pub token: Option<String>,
    /// Where the session descriptor is written. Defaults to
    /// `${HLP_HOME:-~/.hostlink}/<host>/sessions`; overridden by tests so they
    /// never touch the real home directory.
    pub session_dir: Option<PathBuf>,
}

impl Config {
    pub fn loopback(identity: Identity) -> Self {
        Self {
            identity,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            token: None,
            session_dir: None,
        }
    }

    fn is_loopback(&self) -> bool {
        self.bind.is_loopback()
    }
}

#[derive(Clone)]
struct AppState {
    link: Link,
    identity: Identity,
    token: Option<String>,
}

/// A bound, running server.
pub struct Server {
    pub port: u16,
    pub link: Link,
    session_path: Option<PathBuf>,
}

impl Server {
    /// Bind the first free port in [`PORT_RANGE`] and start serving.
    ///
    /// Exhausting the range is an error rather than a fall back to an ephemeral
    /// port: a server nobody can discover is worse than one that failed to
    /// start, and says so.
    pub async fn start(config: Config) -> Result<Self, ServeError> {
        if !config.is_loopback() && config.token.is_none() {
            warn!(
                bind = %config.bind,
                "binding off-loopback without a token; every local and remote \
                 caller can drive the host"
            );
        }

        let (listeners, port) = bind_in_range(config.bind).await?;

        let state = AppState {
            link: Link::new(),
            identity: config.identity.clone(),
            token: config.token.clone(),
        };

        let app = Router::new()
            .route("/hello", get(hello))
            .route("/link", get(upgrade))
            .with_state(state.clone());

        let session_dir = resolve_session_dir(&config);
        if let Some(dir) = &session_dir {
            prune_stale(dir, port).await;
        }

        let session_path = session_dir
            .as_deref()
            .map(|dir| write_session_file(&config.identity, port, dir))
            .transpose()
            .inspect_err(|e| debug!(error = %e, "session descriptor not written"))
            .ok()
            .flatten();

        for listener in listeners {
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app).await {
                    warn!(error = %e, "server stopped");
                }
            });
        }

        info!(port, host = %config.identity.host, "listening");
        Ok(Server { port, link: state.link, session_path })
    }

    /// The URL a host connects to, for logging and for the off-loopback case
    /// where the user has to paste it.
    pub fn connect_url(&self, token: Option<&str>) -> String {
        match token {
            Some(t) => format!("ws://127.0.0.1:{}/link?v={VERSION}&token={t}", self.port),
            None => format!("ws://127.0.0.1:{}/link?v={VERSION}", self.port),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(path) = &self.session_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Bind the first free port in the range, on every address the host may reach
/// us by.
///
/// The host connects to `localhost`, which resolves to `::1` before `127.0.0.1`
/// on most systems. Clients generally fall back to IPv4 when `::1` refuses, but
/// relying on that makes the connection depend on the client's resolver. Both
/// loopback families are bound instead, so either resolution works directly.
///
/// IPv6 is best-effort: a host with it disabled still gets a working server.
async fn bind_in_range(addr: IpAddr) -> Result<(Vec<TcpListener>, u16), ServeError> {
    for port in PORT_RANGE {
        let primary = SocketAddr::new(addr, port);
        let listener = match TcpListener::bind(primary).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(source) => return Err(ServeError::Bind { addr: primary, source }),
        };

        let mut listeners = vec![listener];

        if addr == IpAddr::V4(Ipv4Addr::LOCALHOST) {
            let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port);
            match TcpListener::bind(v6).await {
                Ok(l) => listeners.push(l),
                // The port being taken on ::1 alone means another server holds
                // half the pair; skip it rather than serve an ambiguous port.
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(e) => debug!(error = %e, "no IPv6 loopback; IPv4 only"),
            }
        }

        return Ok((listeners, port));
    }
    Err(ServeError::RangeExhausted)
}

/// Unauthenticated identity probe. The host UI scans the range and lists what
/// answers (§4).
async fn hello(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(Hello {
        identity: state.identity,
        connected: state.link.is_connected().await,
    })
}

async fn upgrade(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    // §9: refuse a major we do not implement rather than negotiating down.
    match params.get("v").map(|v| v.parse::<u32>()) {
        Some(Ok(v)) if v == VERSION => {}
        Some(Ok(v)) => {
            warn!(their = v, ours = VERSION, "refusing connection: protocol version");
            return (
                StatusCode::BAD_REQUEST,
                format!("protocol v{v} not supported; this server speaks v{VERSION}"),
            )
                .into_response();
        }
        _ => return (StatusCode::BAD_REQUEST, "missing or unreadable ?v=").into_response(),
    }

    if let Some(expected) = &state.token {
        let presented = params.get("token").map(String::as_str).unwrap_or_default();
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            warn!("refusing connection: bad token");
            return (StatusCode::UNAUTHORIZED, "bad token").into_response();
        }
    }

    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| pump(socket, state.link))
}

/// Run one host connection until it closes.
async fn pump(socket: ws::WebSocket, link: Link) {
    info!("host connected");
    let (mut sink, mut stream) = socket.split();
    let mut outbound = link.connect().await;

    let writer = tokio::spawn(async move {
        while let Some(body) = outbound.recv().await {
            if let Err(e) = sink.send(ws::Message::Text(body.into())).await {
                debug!(error = %e, "write failed; host has gone");
                break;
            }
        }
    });

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(ws::Message::Text(text)) => link.handle_frame(text.as_str().to_owned()),
            Ok(ws::Message::Close(_)) => break,
            // Ping/Pong are answered by axum; binary frames are not part of the
            // protocol and are ignored rather than treated as an error.
            Ok(_) => {}
            Err(e) => {
                debug!(error = %e, "read failed");
                break;
            }
        }
    }

    writer.abort();
    link.disconnect().await;
    info!("host disconnected");
}

/// Constant-time comparison, so a token cannot be recovered by timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 32 bytes of CSPRNG output, hex encoded (§5).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("system CSPRNG");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hostlink_home() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HLP_HOME") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    Some(PathBuf::from(home).join(".hostlink"))
}

fn resolve_session_dir(config: &Config) -> Option<PathBuf> {
    config.session_dir.clone().or_else(|| {
        Some(hostlink_home()?.join(&config.identity.host).join("sessions"))
    })
}

/// Remove descriptors that no longer describe a running server.
///
/// A process that is killed rather than shut down never runs `Drop`, so its
/// descriptor outlives it. Staleness is decided by whether anything still
/// answers on the recorded port rather than by whether the pid exists: pids are
/// recycled, ports are what actually matter, and a descriptor naming a port we
/// just bound ourselves is stale by definition.
async fn prune_stale(dir: &std::path::Path, our_port: u16) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let Ok(body) = std::fs::read(&path) else { continue };
        let Ok(session) = serde_json::from_slice::<SessionFile>(&body) else {
            // Unreadable descriptors are junk too.
            let _ = std::fs::remove_file(&path);
            continue;
        };

        if session.identity.pid == std::process::id() {
            continue;
        }

        let dead = session.port == our_port || !port_answers(session.port).await;
        if dead {
            debug!(port = session.port, pid = session.identity.pid, "pruning stale descriptor");
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Whether anything accepts a connection on this loopback port.
async fn port_answers(port: u16) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            tokio::net::TcpStream::connect(addr),
        )
        .await,
        Ok(Ok(_))
    )
}

fn write_session_file(
    identity: &Identity,
    port: u16,
    dir: &std::path::Path,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;

    // Keyed by pid *and* port: one process may run several servers, and a
    // pid-only name would have them overwrite and then delete each other's
    // descriptors.
    let path = dir.join(format!("{}-{}.json", identity.pid, port));
    let body = serde_json::to_vec_pretty(&SessionFile { identity: identity.clone(), port })
        .map_err(std::io::Error::other)?;
    std::fs::write(&path, body)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_semantics_of_normal_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn generated_tokens_are_64_hex_chars_and_distinct() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "tokens must not repeat across calls");
    }

    #[test]
    fn range_is_twenty_slots() {
        assert_eq!(PORT_RANGE.count(), 20);
    }

    /// A config whose session descriptor lands in a scratch directory rather
    /// than the developer's home.
    fn test_config(label: &str, dir: &std::path::Path) -> Config {
        Config { session_dir: Some(dir.to_path_buf()), ..Config::loopback(Identity::new("figma", label)) }
    }

    #[tokio::test]
    async fn hello_reports_identity_and_link_is_reachable() {
        let tmp = std::env::temp_dir().join(format!("hostlink-test-{}", std::process::id()));
        let server = Server::start(test_config("test", &tmp)).await.expect("start");
        assert!(PORT_RANGE.contains(&server.port));

        let body = reqwest_get(&format!("http://127.0.0.1:{}/hello", server.port)).await;
        let hello: Hello = serde_json::from_str(&body).expect("hello json");
        assert_eq!(hello.identity.host, "figma");
        assert_eq!(hello.identity.label, "test");
        assert_eq!(hello.identity.v, VERSION);
        assert_eq!(hello.identity.pid, std::process::id());
        assert!(!hello.connected, "no host has attached yet");
    }

    /// A descriptor left behind by a killed process must not outlive it.
    #[tokio::test]
    async fn stale_descriptors_are_pruned_on_start() {
        let tmp = std::env::temp_dir().join(format!("hostlink-prune-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // A server that died on a port nothing listens on any more. The pid
        // must not be ours: a descriptor bearing the running process's pid is
        // deliberately left alone.
        let mut identity = Identity::new("figma", "ghost");
        identity.pid = u32::MAX;
        // Port 5 is outside the range, so nothing can be listening on it.
        let ghost = SessionFile { identity, port: 5 };
        let ghost_path = tmp.join("999999-5.json");
        std::fs::write(&ghost_path, serde_json::to_vec(&ghost).unwrap()).unwrap();
        std::fs::write(tmp.join("garbage.json"), b"not json").unwrap();

        let server = Server::start(test_config("live", &tmp)).await.expect("start");

        assert!(!ghost_path.exists(), "descriptor for a dead port should be gone");
        assert!(!tmp.join("garbage.json").exists(), "unreadable descriptor should be gone");

        let remaining: Vec<_> = std::fs::read_dir(&tmp).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(remaining.len(), 1, "only the live server's own descriptor should survive");
        drop(server);
    }

    /// Two servers must not contend: the second takes the next free slot.
    #[tokio::test]
    async fn a_second_server_takes_the_next_port() {
        let tmp = std::env::temp_dir().join(format!("hostlink-test2-{}", std::process::id()));
        let first = Server::start(test_config("one", &tmp)).await.expect("first");
        let second = Server::start(test_config("two", &tmp)).await.expect("second");
        assert_ne!(first.port, second.port);
        assert!(PORT_RANGE.contains(&second.port));

        // Both descriptors must coexist; a pid-only filename would collide.
        let written: Vec<_> = std::fs::read_dir(&tmp).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(written.len(), 2, "each server needs its own descriptor");
    }

    /// Minimal HTTP GET, to avoid pulling a client dependency into the crate
    /// for the sake of two tests.
    async fn reqwest_get(url: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.trim_start_matches("http://");
        let (authority, path) = rest.split_once('/').unwrap();
        let mut sock = tokio::net::TcpStream::connect(authority).await.expect("connect");
        sock.write_all(
            format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("write");
        let mut raw = String::new();
        sock.read_to_string(&mut raw).await.expect("read");
        raw.split_once("\r\n\r\n").expect("headers").1.to_owned()
    }
}
