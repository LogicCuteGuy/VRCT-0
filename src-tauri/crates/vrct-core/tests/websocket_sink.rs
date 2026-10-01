//! The WebSocket sink against real clients: token gate, verbatim broadcast,
//! lifecycle (stop, restart on the same port, wildcard refusal).

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::sinks::Sinks;

const TOKEN: &str = "secret-token_123";
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn line(endpoint: &str, result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": endpoint, "result": result}).to_string();
    parse_sidecar_line(&text).unwrap()
}

fn sinks(token: Option<&str>) -> Sinks {
    let replica = Arc::new(ConfigReplica::default());
    let mut config = json!({"OSC_IP_ADDRESS": "127.0.0.1", "OSC_PORT": 9000});
    if let Some(token) = token {
        config["WEBSOCKET_AUTH_TOKEN"] = json!(token);
    }
    replica.ingest(&line("/internal/config/snapshot", config));
    Sinks::new(replica)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start(sinks: &Sinks, port: u16) {
    assert!(sinks.ingest(&line("/internal/websocket/start", json!({"host": "127.0.0.1", "port": port}))));
}

fn broadcast(sinks: &Sinks, text: &str) {
    assert!(sinks.ingest(&line("/internal/websocket/broadcast", json!({"text": text}))));
}

/// Connect, waiting for the server to finish binding in the background.
async fn connect(port: u16, query: &str) -> Result<Client, Error> {
    let url = format!("ws://127.0.0.1:{port}/{query}");
    let mut last = None;
    for _ in 0..50 {
        match connect_async(&url).await {
            Ok((client, _)) => return Ok(client),
            // A refused connection means "not listening yet"; anything else is
            // the server's real answer.
            Err(Error::Io(error)) => last = Some(Error::Io(error)),
            Err(other) => return Err(other),
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    Err(last.unwrap())
}

async fn next_text(client: &mut Client) -> String {
    match timeout(Duration::from_secs(2), client.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text.to_string(),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

fn is_forbidden(result: Result<Client, Error>) -> bool {
    matches!(result, Err(Error::Http(response)) if response.status() == 403)
}

#[tokio::test]
async fn clients_without_the_right_token_are_refused() {
    let sinks = sinks(Some(TOKEN));
    let port = free_port();
    start(&sinks, port);

    assert!(is_forbidden(connect(port, "").await), "no token");
    assert!(is_forbidden(connect(port, "?token=wrong").await), "wrong token");
    assert!(is_forbidden(connect(port, "?other=1").await), "no token parameter");
    assert!(connect(port, &format!("?token={TOKEN}")).await.is_ok(), "right token");
}

#[tokio::test]
async fn without_a_token_on_record_nobody_gets_in() {
    let sinks = sinks(None);
    let port = free_port();
    start(&sinks, port);
    assert!(is_forbidden(connect(port, "?token=").await));
    assert!(is_forbidden(connect(port, "?token=anything").await));
}

#[tokio::test]
async fn every_client_gets_the_broadcast_verbatim() {
    let sinks = sinks(Some(TOKEN));
    let port = free_port();
    start(&sinks, port);
    let query = format!("?token={TOKEN}");
    let mut a = connect(port, &query).await.unwrap();
    let mut b = connect(port, &query).await.unwrap();

    // Python's own json.dumps output, escapes and all, must arrive untouched.
    let text = r#"{"type": "SEND", "message": "こんにちは", "translation": ["hi"]}"#;
    broadcast(&sinks, text);
    broadcast(&sinks, "second");

    assert_eq!(next_text(&mut a).await, text);
    assert_eq!(next_text(&mut b).await, text);
    assert_eq!(next_text(&mut a).await, "second");
    assert_eq!(next_text(&mut b).await, "second");
}

#[tokio::test]
async fn token_may_be_percent_encoded_in_the_url() {
    let sinks = sinks(Some("a+b c"));
    let port = free_port();
    start(&sinks, port);
    assert!(connect(port, "?token=a%2Bb%20c").await.is_ok());
}

#[tokio::test]
async fn stop_closes_clients_and_frees_the_port() {
    let sinks = sinks(Some(TOKEN));
    let port = free_port();
    start(&sinks, port);
    let mut client = connect(port, &format!("?token={TOKEN}")).await.unwrap();

    assert!(sinks.ingest(&line("/internal/websocket/stop", Value::Null)));
    let end = timeout(Duration::from_secs(2), client.next()).await.expect("closes in time");
    assert!(!matches!(end, Some(Ok(Message::Text(_)))), "got {end:?}");

    // Broadcasting while stopped is harmless, and the port is free again.
    broadcast(&sinks, "nobody listens");
    let mut rebound = false;
    for _ in 0..50 {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            rebound = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(rebound, "port {port} was not released");
}

#[tokio::test]
async fn stop_then_start_on_the_same_port_works() {
    // Python does exactly this when a setting changes while the server runs.
    let sinks = sinks(Some(TOKEN));
    let port = free_port();
    let query = format!("?token={TOKEN}");
    start(&sinks, port);
    let _first = connect(port, &query).await.unwrap();

    sinks.ingest(&line("/internal/websocket/stop", Value::Null));
    start(&sinks, port);
    let mut second = connect(port, &query).await.unwrap();
    broadcast(&sinks, "again");
    assert_eq!(next_text(&mut second).await, "again");
}

#[tokio::test]
async fn starting_on_another_address_moves_the_server() {
    let sinks = sinks(Some(TOKEN));
    let (old, new) = (free_port(), free_port());
    let query = format!("?token={TOKEN}");
    start(&sinks, old);
    let _client = connect(old, &query).await.unwrap();

    start(&sinks, new);
    let mut moved = connect(new, &query).await.unwrap();
    broadcast(&sinks, "here");
    assert_eq!(next_text(&mut moved).await, "here");
}

#[tokio::test]
async fn a_wildcard_address_is_refused() {
    // A fresh sink per host: a second start would otherwise replace the first
    // server and hide whether the first was ever accepted.
    for (host, probe) in [("0.0.0.0", "127.0.0.1"), ("::", "::1")] {
        let sinks = sinks(Some(TOKEN));
        let port = free_port();
        assert!(sinks.ingest(&line("/internal/websocket/start", json!({"host": host, "port": port}))));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            TcpStream::connect((probe, port)).await.is_err(),
            "something is listening after start on {host}"
        );
    }
}

#[tokio::test]
async fn malformed_lines_are_consumed_without_effect() {
    let sinks = sinks(Some(TOKEN));
    assert!(sinks.ingest(&line("/internal/websocket/start", json!({"host": "127.0.0.1"}))));
    assert!(sinks.ingest(&line("/internal/websocket/start", json!({"host": "127.0.0.1", "port": 99999}))));
    assert!(sinks.ingest(&line("/internal/websocket/broadcast", json!({"oops": 1}))));
}

#[test]
fn starting_outside_a_runtime_is_an_error_not_a_panic() {
    let sinks = sinks(Some(TOKEN));
    // Consumed (true) and reported on stderr; the point is it must not panic.
    assert!(sinks.ingest(&line("/internal/websocket/start", json!({"host": "127.0.0.1", "port": 1}))));
}
