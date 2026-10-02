//! The HTTP policy shared by the cloud engines: a bounded wait and a few
//! retries for failures worth retrying.
//!
//! Errors are `HTTP <status>[: provider message]` or `request failed: ...`.
//! They never contain the URL or a key, since they travel back to Python and
//! into its log.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::{Client, Method, Response, StatusCode};
use serde_json::Value;

/// One attempt, including the provider's time to answer. Python's OpenAI client
/// waited up to ten minutes, which would stall a live conversation. A timed-out
/// request is not retried: it would only time out again, doubling the wait.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// First try plus two retries, as the OpenAI SDK does by default.
const ATTEMPTS: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

pub type Headers<'a> = &'a [(&'static str, String)];

/// A request that did not get an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError {
    /// The wait ran out (as opposed to a refused or broken connection).
    pub timed_out: bool,
    pub message: String,
}

enum Body<'a> {
    None,
    Json(&'a Value),
    Bytes { content_type: &'a str, data: &'a [u8] },
}

/// What a server answered, undecoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

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

/// Send until a reply that is final: a success, a status not worth retrying, or
/// the last attempt. Only transport failures are errors here.
async fn send(
    method: Method,
    url: &str,
    headers: Headers<'_>,
    query: &[(&str, String)],
    body: Body<'_>,
    timeout: Option<Duration>,
    attempts: u32,
) -> Result<Response, SendError> {
    let mut backoff = FIRST_BACKOFF;
    for attempt in 1..=attempts {
        let mut builder = client().request(method.clone(), url);
        if !query.is_empty() {
            builder = builder.query(query);
        }
        match &body {
            Body::None => {}
            Body::Json(json) => builder = builder.json(json),
            Body::Bytes { content_type, data } => {
                builder = builder.header("Content-Type", *content_type).body(data.to_vec());
            }
        }
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        let last = attempt == attempts;
        match builder.send().await {
            Ok(response) => {
                if response.status().is_success() || !retryable(response.status()) || last {
                    return Ok(response);
                }
            }
            Err(error) => {
                if last || !error.is_connect() {
                    return Err(SendError {
                        timed_out: error.is_timeout(),
                        message: format!("request failed: {}", error.without_url()),
                    });
                }
            }
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    unreachable!("the last attempt always returns")
}

async fn json_of(response: Response) -> Result<Value, String> {
    let status = response.status();
    if status.is_success() {
        return response.json().await.map_err(|e| format!("bad reply: {}", e.without_url()));
    }
    let body = response.text().await.unwrap_or_default();
    Err(match failure_message(&body) {
        Some(message) => format!("HTTP {}: {message}", status.as_u16()),
        None => format!("HTTP {}", status.as_u16()),
    })
}

/// POST `body` as JSON and return the JSON reply.
pub async fn post_json(url: &str, headers: Headers<'_>, body: &Value) -> Result<Value, String> {
    json_of(send(Method::POST, url, headers, &[], Body::Json(body), None, ATTEMPTS).await.map_err(|e| e.message)?).await
}

/// GET with the cloud policy (retries included) and return the JSON reply.
pub async fn get_json(url: &str, headers: Headers<'_>) -> Result<Value, String> {
    json_of(send(Method::GET, url, headers, &[], Body::None, None, ATTEMPTS).await.map_err(|e| e.message)?).await
}

/// One GET without retries and with its own wait: a local server, where nothing
/// is worth waiting on, or a key check whose status alone is the answer. An
/// unreachable server is an error, any answer is returned as its status.
pub async fn get_status_once(url: &str, headers: Headers<'_>, timeout: Duration) -> Result<u16, String> {
    Ok(send(Method::GET, url, headers, &[], Body::None, Some(timeout), 1).await.map_err(|e| e.message)?.status().as_u16())
}

/// Like `get_status_once`, returning the JSON reply of a successful answer.
pub async fn get_json_once(url: &str, headers: Headers<'_>, timeout: Duration) -> Result<Value, String> {
    json_of(send(Method::GET, url, headers, &[], Body::None, Some(timeout), 1).await.map_err(|e| e.message)?).await
}

async fn reply_of(response: Response) -> Result<Reply, SendError> {
    let status = response.status().as_u16();
    match response.bytes().await {
        Ok(body) => Ok(Reply { status, body: body.to_vec() }),
        Err(error) => Err(SendError { timed_out: error.is_timeout(), message: format!("bad reply: {}", error.without_url()) }),
    }
}

/// POST raw bytes (with the cloud policy when `attempts` > 1) and return whatever status and body come
/// back, for callers that map statuses themselves.
pub async fn post_bytes(
    url: &str,
    headers: Headers<'_>,
    query: &[(&str, String)],
    content_type: &str,
    data: &[u8],
    timeout: Duration,
    attempts: u32,
) -> Result<Reply, SendError> {
    let body = Body::Bytes { content_type, data };
    reply_of(send(Method::POST, url, headers, query, body, Some(timeout), attempts).await?).await
}

/// GET and return the status and body, whatever the status.
pub async fn get_reply(url: &str, headers: Headers<'_>, timeout: Duration, attempts: u32) -> Result<Reply, SendError> {
    reply_of(send(Method::GET, url, headers, &[], Body::None, Some(timeout), attempts).await?).await
}
