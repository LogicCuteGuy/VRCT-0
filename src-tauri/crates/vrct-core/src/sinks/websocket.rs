//! Local WebSocket server that broadcasts transcription/translation messages
//! (OBS Browser Source, VRCT-TTS and other tools connect to it).
//!
//! Python still decides *when* it runs (port probe, enable/disable, the
//! wildcard-address guard) and sends `start`/`stop`; Rust owns the sockets.
//! Every client must present `?token=<WEBSOCKET_AUTH_TOKEN>` in the URL:
//! WebSocket is exempt from the same-origin policy, so without it any web page
//! on this PC could read the user's speech.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

use super::net::{bind, is_wildcard};
use crate::config::ConfigReplica;

const TOKEN_KEY: &str = "WEBSOCKET_AUTH_TOKEN";
/// A client this far behind is skipped rather than slowing the others.
const BACKLOG: usize = 256;

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&high), Some(&low)) => {
                (high as char).to_digit(16).zip((low as char).to_digit(16))
            }
            _ => None,
        };
        match (bytes[i], escaped) {
            (_, Some((high, low))) => {
                out.push((high * 16 + low) as u8);
                i += 2;
            }
            (b'+', _) => out.push(b' '),
            (byte, _) => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first `token` query parameter, decoded like Python's `parse_qs`.
pub fn token_from_query(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(key, _)| percent_decode(key) == "token")
        .map(|(_, value)| percent_decode(value))
}

fn forbidden() -> ErrorResponse {
    let mut response = ErrorResponse::new(Some("Forbidden: invalid or missing token\n".into()));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
}

struct Running {
    host: String,
    port: u16,
    shutdown: watch::Sender<bool>,
    messages: broadcast::Sender<Arc<str>>,
}

pub struct WebSocketSink {
    replica: Arc<ConfigReplica>,
    running: Mutex<Option<Running>>,
}

impl WebSocketSink {
    pub fn new(replica: Arc<ConfigReplica>) -> Self {
        Self {
            replica,
            running: Mutex::new(None),
        }
    }

    /// Start listening (replacing a server on another address). Must be
    /// called from inside a Tokio runtime. The bind itself happens in the
    /// background and is reported on stderr if it fails.
    pub fn start(&self, host: &str, port: u16) -> Result<(), String> {
        self.start_impl(host, port, false)
    }

    /// Native controller path: reserve the new socket before releasing the old
    /// server, so a busy port cannot produce a successful UI response.
    pub fn start_checked(&self, host: &str, port: u16) -> Result<(), String> {
        let bound = self.prepare(host, port)?;
        self.start_prepared(host, port, bound)
    }

    pub(crate) fn prepare(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Option<tokio::net::TcpListener>, String> {
        if is_wildcard(host) {
            return Err(format!(
                "refusing to serve WebSocket on wildcard address {host}"
            ));
        }
        if self
            .running
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| r.host == host && r.port == port)
        {
            return Ok(None);
        }
        let socket = std::net::TcpListener::bind((host, port))
            .map_err(|e| format!("WebSocket bind failed: {e}"))?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        tokio::net::TcpListener::from_std(socket)
            .map(Some)
            .map_err(|e| e.to_string())
    }
    pub(crate) fn start_prepared(
        &self,
        host: &str,
        port: u16,
        bound: Option<tokio::net::TcpListener>,
    ) -> Result<(), String> {
        self.start_with(host, port, bound)
    }

    fn start_impl(&self, host: &str, port: u16, checked: bool) -> Result<(), String> {
        let bound = if checked {
            self.prepare(host, port)?
        } else {
            None
        };
        self.start_with(host, port, bound)
    }
    fn start_with(
        &self,
        host: &str,
        port: u16,
        listener: Option<tokio::net::TcpListener>,
    ) -> Result<(), String> {
        if is_wildcard(host) {
            return Err(format!(
                "refusing to serve WebSocket on wildcard address {host}"
            ));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "WebSocket sink needs a Tokio runtime".to_string())?;
        let mut running = self.running.lock().unwrap();
        if running
            .as_ref()
            .is_some_and(|r| r.host == host && r.port == port)
        {
            return Ok(());
        }
        if let Some(previous) = running.take() {
            let _ = previous.shutdown.send(true);
        }
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (messages, _) = broadcast::channel(BACKLOG);
        runtime.spawn(serve(
            host.to_string(),
            port,
            Arc::clone(&self.replica),
            shutdown_rx,
            messages.clone(),
            listener,
        ));
        *running = Some(Running {
            host: host.to_string(),
            port,
            shutdown,
            messages,
        });
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            let _ = running.shutdown.send(true);
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    /// Send `text` verbatim to every connected client.
    pub fn broadcast(&self, text: &str) {
        if let Some(running) = self.running.lock().unwrap().as_ref() {
            // No receivers just means nobody is connected.
            let _ = running.messages.send(Arc::from(text));
        }
    }
}

async fn serve(
    host: String,
    port: u16,
    replica: Arc<ConfigReplica>,
    mut shutdown: watch::Receiver<bool>,
    messages: broadcast::Sender<Arc<str>>,
    bound: Option<tokio::net::TcpListener>,
) {
    let listener = match if let Some(listener) = bound {
        Ok(listener)
    } else {
        bind(&host, port, &mut shutdown).await
    } {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("[sinks] websocket: {error}");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(client(
                        stream,
                        Arc::clone(&replica),
                        shutdown.clone(),
                        messages.subscribe(),
                    ));
                }
                Err(error) => eprintln!("[sinks] websocket accept: {error}"),
            },
        }
    }
}

#[allow(clippy::result_large_err)] // tungstenite fixes the handshake callback's error type
async fn client(
    stream: TcpStream,
    replica: Arc<ConfigReplica>,
    mut shutdown: watch::Receiver<bool>,
    mut messages: broadcast::Receiver<Arc<str>>,
) {
    // Read per connection so a regenerated token applies to new clients. No
    // token on record means nobody can be verified, so nobody gets in.
    let expected = replica.get_str(TOKEN_KEY);
    let check = |request: &Request, response: Response| match &expected {
        Some(expected) if token_from_query(request.uri().query()).as_deref() == Some(expected) => {
            Ok(response)
        }
        _ => Err(forbidden()),
    };
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, check).await else {
        return;
    };
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                let _ = socket.close(None).await;
                return;
            }
            message = messages.recv() => match message {
                Ok(text) => {
                    if socket.send(Message::text(text.to_string())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            // Clients never send anything we act on, but reading is what
            // answers pings and notices a close.
            incoming = socket.next() => match incoming {
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_read_like_parse_qs() {
        assert_eq!(token_from_query(Some("token=abc")).as_deref(), Some("abc"));
        assert_eq!(
            token_from_query(Some("a=1&token=x-y_z&b=2")).as_deref(),
            Some("x-y_z")
        );
        assert_eq!(
            token_from_query(Some("token=a%2Bb%20c+d")).as_deref(),
            Some("a+b c d")
        );
        // The first one wins, as `parse_qs(...)["token"][0]` does.
        assert_eq!(
            token_from_query(Some("token=1&token=2")).as_deref(),
            Some("1")
        );
        assert_eq!(token_from_query(Some("token=")).as_deref(), Some(""));
        assert_eq!(token_from_query(Some("tokens=abc")), None);
        assert_eq!(token_from_query(Some("")), None);
        assert_eq!(token_from_query(None), None);
    }

    #[test]
    fn a_broken_escape_is_kept_literally() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%+f"), "% f");
        assert_eq!(percent_decode("%é"), "%é");
    }

    #[test]
    fn start_refuses_wildcard_hosts_before_touching_the_network() {
        let sink = WebSocketSink::new(Arc::new(ConfigReplica::default()));
        for host in ["0.0.0.0", "::"] {
            let error = sink.start(host, 8765).unwrap_err();
            assert!(error.contains("wildcard"), "{error}");
        }
    }
}
