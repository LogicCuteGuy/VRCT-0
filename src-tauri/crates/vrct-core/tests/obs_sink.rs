//! The OBS browser-source server against real HTTP clients.
//! `fixtures/obs_golden.json` is captured from the real Python page builder
//! and a real running Python server by `fixtures/regenerate_obs_golden.py`.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::sinks::obs::render_page;
use vrct_core::sinks::Sinks;

const GOLDEN: &str = include_str!("fixtures/obs_golden.json");

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("golden fixture is JSON")
}

fn line(endpoint: &str, result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": endpoint, "result": result}).to_string();
    parse_sidecar_line(&text).expect("sidecar line parses")
}

fn replica_with(config: Value, token: &str) -> Arc<ConfigReplica> {
    let replica = Arc::new(ConfigReplica::default());
    let mut config = config;
    config["WEBSOCKET_AUTH_TOKEN"] = json!(token);
    replica.ingest(&line("/internal/config/snapshot", config));
    replica
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start(sinks: &Sinks, port: u16) {
    assert!(sinks.ingest(&line("/internal/obs/start", json!({"host": "127.0.0.1", "port": port}))));
}

struct Http {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Http {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Send raw bytes and read the whole reply, waiting for the server to bind.
async fn exchange(port: u16, request: &str) -> Http {
    let mut stream = None;
    for _ in 0..50 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(40)).await,
        }
    }
    let mut stream = stream.expect("server is listening");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("server closes the connection")
        .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").expect("a complete response");
    let mut lines = head.lines();
    let status = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Http { status, headers, body: body.to_string() }
}

async fn get(port: u16, path: &str) -> Http {
    exchange(port, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")).await
}

#[test]
fn the_page_is_what_python_rendered_for_every_golden_case() {
    let golden = golden();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 8, "fixture looks empty");
    for case in cases {
        let replica = replica_with(case["config"].clone(), case["token"].as_str().unwrap());
        let expected = case["html"].as_str().unwrap();
        assert!(render_page(&replica) == expected, "page differs for case {}", case["name"]);
    }
}

#[tokio::test]
async fn http_answers_like_the_real_python_server() {
    let replica = replica_with(json!({}), "tok");
    let sinks = Sinks::new(Arc::clone(&replica));
    let port = free_port();
    start(&sinks, port);

    let golden = golden();
    for probe in golden["http"].as_array().unwrap() {
        let path = probe["path"].as_str().unwrap();
        let reply = get(port, path).await;
        assert_eq!(reply.status as u64, probe["status"].as_u64().unwrap(), "status of {path}");
        assert_eq!(reply.header("content-type"), probe["content_type"].as_str(), "content type of {path}");
        assert_eq!(reply.header("cache-control"), probe["cache_control"].as_str(), "cache control of {path}");
        if probe["body_is_page"].as_bool().unwrap() {
            assert_eq!(reply.body, render_page(&replica), "page body of {path}");
            let length: usize = reply.header("content-length").unwrap().parse().unwrap();
            assert_eq!(length, reply.body.len(), "content length of {path}");
        } else {
            assert_eq!(reply.body, probe["body"].as_str().unwrap(), "body of {path}");
        }
    }
}

#[tokio::test]
async fn the_page_follows_the_config_and_the_token_at_request_time() {
    let replica = replica_with(json!({"OBS_BROWSER_SOURCE_FONT_SIZE": 40}), "first-token");
    let sinks = Sinks::new(Arc::clone(&replica));
    let port = free_port();
    start(&sinks, port);

    let before = get(port, "/").await.body;
    assert!(before.contains("--font-size: 40px;") && before.contains(r#"wsToken: "first-token""#));

    assert!(replica.ingest(&line(
        "/internal/config/changed",
        json!({"key": "OBS_BROWSER_SOURCE_FONT_SIZE", "value": 77})
    )));
    assert!(replica.ingest(&line(
        "/internal/config/changed",
        json!({"key": "WEBSOCKET_AUTH_TOKEN", "value": "second-token"})
    )));
    let after = get(port, "/obs").await.body;
    assert!(after.contains("--font-size: 77px;") && after.contains(r#"wsToken: "second-token""#), "{after}");
}

#[tokio::test]
async fn other_methods_and_junk_are_refused_politely() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    let port = free_port();
    start(&sinks, port);

    // Python's BaseHTTPRequestHandler answers 501 to anything but GET, HEAD included.
    for method in ["HEAD", "POST", "DELETE"] {
        let reply = exchange(port, &format!("{method} / HTTP/1.1\r\nHost: x\r\n\r\n")).await;
        assert_eq!(reply.status, 501, "{method}");
    }
    assert_eq!(exchange(port, "nonsense\r\n\r\n").await.status, 400);
    // A GET with an absolute-form target still routes.
    assert_eq!(get(port, "http://localhost/health").await.body, "ok");
}

#[tokio::test]
async fn an_endless_request_head_is_dropped_not_buffered_forever() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    let port = free_port();
    start(&sinks, port);
    // Make sure the server is up first.
    assert_eq!(get(port, "/health").await.status, 200);

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let junk = vec![b'a'; 64 * 1024];
    let _ = stream.write_all(&junk).await;
    let mut rest = Vec::new();
    let end = timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    assert!(end.is_ok(), "the server kept the connection open");
    assert!(rest.is_empty(), "no reply is owed to a head that never ends");
    // And it still serves others.
    assert_eq!(get(port, "/health").await.status, 200);
}

#[tokio::test]
async fn many_clients_are_served_at_once() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    let port = free_port();
    start(&sinks, port);
    let requests = (0..24).map(|_| get(port, "/obs"));
    let replies = futures_util::future::join_all(requests).await;
    assert!(replies.iter().all(|r| r.status == 200 && r.body.starts_with("<!doctype html>")));
}

#[tokio::test]
async fn stop_frees_the_port_and_start_works_again_on_it() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    let port = free_port();
    start(&sinks, port);
    assert_eq!(get(port, "/health").await.status, 200);

    assert!(sinks.ingest(&line("/internal/obs/stop", Value::Null)));
    let mut freed = false;
    for _ in 0..50 {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(freed, "port {port} was not released");

    // Python does exactly this when the port setting changes while it runs.
    start(&sinks, port);
    assert_eq!(get(port, "/health").await.status, 200);
}

#[tokio::test]
async fn starting_on_another_port_moves_the_server() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    let (old, new) = (free_port(), free_port());
    start(&sinks, old);
    assert_eq!(get(old, "/health").await.status, 200);

    start(&sinks, new);
    assert_eq!(get(new, "/health").await.status, 200);
    let mut old_closed = false;
    for _ in 0..50 {
        if TcpListener::bind(("127.0.0.1", old)).is_ok() {
            old_closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(old_closed, "the old port is still held");
}

#[tokio::test]
async fn a_wildcard_address_is_refused() {
    for (host, probe) in [("0.0.0.0", "127.0.0.1"), ("::", "::1")] {
        let sinks = Sinks::new(replica_with(json!({}), "tok"));
        let port = free_port();
        assert!(sinks.ingest(&line("/internal/obs/start", json!({"host": host, "port": port}))));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            TcpStream::connect((probe, port)).await.is_err(),
            "something is listening after start on {host}"
        );
    }
}

#[tokio::test]
async fn malformed_lines_are_consumed_without_effect() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    assert!(sinks.ingest(&line("/internal/obs/start", json!({"host": "127.0.0.1"}))));
    assert!(sinks.ingest(&line("/internal/obs/start", json!({"host": "127.0.0.1", "port": 99999}))));
    assert!(sinks.ingest(&line("/internal/obs/stop", Value::Null)));
}

#[test]
fn starting_outside_a_runtime_is_an_error_not_a_panic() {
    let sinks = Sinks::new(replica_with(json!({}), "tok"));
    assert!(sinks.ingest(&line("/internal/obs/start", json!({"host": "127.0.0.1", "port": 1}))));
}
