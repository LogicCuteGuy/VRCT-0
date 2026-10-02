//! A scripted HTTP server for tests: records every request and answers with
//! the next canned reply (the last one repeats).

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Captured {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl Captured {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct Mock {
    pub port: u16,
    pub captured: Arc<Mutex<Vec<Captured>>>,
}

impl Mock {
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }
}

pub async fn mock(replies: Vec<(u16, String)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&captured);
    tokio::spawn(async move {
        let mut served = 0usize;
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                let n = stream.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break None;
                }
                raw.extend_from_slice(&chunk[..n]);
                if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(at + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or_default().to_string();
            let headers: Vec<(String, String)> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            let length = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            while raw.len() < head_end + length {
                let n = stream.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
            }
            let body = serde_json::from_slice(&raw[head_end..]).unwrap_or(Value::Null);
            log.lock().unwrap().push(Captured { request_line, headers, body });

            let (status, text) = &replies[served.min(replies.len() - 1)];
            served += 1;
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    Mock { port, captured }
}
