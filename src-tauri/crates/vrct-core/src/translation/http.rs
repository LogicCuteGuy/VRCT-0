//! The HTTP policy shared by every cloud translation engine: one JSON POST
//! with a bounded wait and a few retries for failures worth retrying.
//!
//! Errors are `HTTP <status>[: provider message]` or `request failed: ...`.
//! They never contain the URL or a key, since they travel back to Python and
//! into its log.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::Value;

/// One attempt, including the provider's time to answer. Python's OpenAI client
/// waited up to ten minutes, which would stall a live conversation. A timed-out
/// request is not retried: it would only time out again, doubling the wait.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// First try plus two retries, as the OpenAI SDK does by default.
const ATTEMPTS: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(TOTAL_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("HTTP client builds")
    })
}

fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error()
}

/// The provider's own explanation of a failure, if its body has one. OpenAI and
/// Gemini nest it under `error`, Ollama puts a string there, DeepL uses `message`.
fn failure_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let message = value
        .pointer("/error/message")
        .or_else(|| value.get("error"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)?;
    Some(message.chars().take(200).collect())
}

/// POST `body` as JSON and return the JSON reply.
pub async fn post_json(url: &str, headers: &[(&'static str, String)], body: &Value) -> Result<Value, String> {
    let mut backoff = FIRST_BACKOFF;
    for attempt in 1..=ATTEMPTS {
        let mut builder = client().post(url).json(body);
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        let last = attempt == ATTEMPTS;
        match builder.send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return response.json().await.map_err(|e| format!("bad reply: {}", e.without_url()));
                }
                if !retryable(status) || last {
                    let body = response.text().await.unwrap_or_default();
                    return Err(match failure_message(&body) {
                        Some(message) => format!("HTTP {}: {message}", status.as_u16()),
                        None => format!("HTTP {}", status.as_u16()),
                    });
                }
            }
            Err(error) => {
                if last || !error.is_connect() {
                    return Err(format!("request failed: {}", error.without_url()));
                }
            }
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    unreachable!("the last attempt always returns")
}
