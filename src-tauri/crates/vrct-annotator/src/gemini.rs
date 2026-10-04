//! Direct Gemini REST. Retries are journalled here, never hidden in the client.
use crate::{filesystem as fsx, job, Result};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

pub struct AnnotateOptions {
    pub limit: usize,
    pub interval: f64,
    pub retries: u8,
    pub retry_failed: bool,
    /// Localhost origin only, for offline HTTP regression tests.
    pub endpoint: Option<String>,
}
impl Default for AnnotateOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            interval: 6.,
            retries: 2,
            retry_failed: false,
            endpoint: None,
        }
    }
}
pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}
struct RealSleep;
impl Sleeper for RealSleep {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

pub fn request_body(data: &[u8]) -> Value {
    json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(data)}},{"text":job::PROMPT}]}],"generationConfig":{"responseMimeType":"application/json","responseJsonSchema":job::schema(),"candidateCount":1,"temperature":0,"maxOutputTokens":8192}})
}
pub fn retry_delay(header: Option<&str>, attempt: u8, now: SystemTime) -> Duration {
    if let Some(header) = header {
        if let Ok(number) = header.parse::<f64>() {
            if number.is_finite() && number > 0. {
                if let Ok(delay) = Duration::try_from_secs_f64(number) {
                    return delay;
                }
            }
        } else if let Ok(date) = httpdate::parse_http_date(header) {
            if let Ok(delay) = date.duration_since(now) {
                if !delay.is_zero() {
                    return delay;
                }
            }
        }
    }
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    Duration::from_secs_f64(
        2f64.powi(i32::from(attempt)).min(60.)
            + u64::from_le_bytes(bytes) as f64 / (u64::MAX as f64 + 1.),
    )
}
fn origin(override_origin: Option<&str>) -> Result<String> {
    let Some(origin) = override_origin else {
        return Ok("https://generativelanguage.googleapis.com".into());
    };
    let url = reqwest::Url::parse(origin).map_err(|_| "invalid test endpoint")?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("endpoint overrides are restricted to localhost origins".into());
    }
    Ok(origin.trim_end_matches('/').to_owned())
}
fn snake_case(field: &str) -> String {
    let mut result = String::new();
    for c in field.chars() {
        if c.is_ascii_uppercase() {
            result.push('_');
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}
fn contains_secret(value: &Value, key: &str) -> bool {
    match value {
        Value::String(text) => text.contains(key),
        Value::Array(values) => values.iter().any(|value| contains_secret(value, key)),
        Value::Object(values) => values
            .iter()
            .any(|(name, value)| name.contains(key) || contains_secret(value, key)),
        _ => false,
    }
}
fn normalize_response(response: Value, key: &str) -> Value {
    // An unexpected proxy response must not persist credentials, even when it
    // arrives with HTTP 200. Transport/error body text is never recorded.
    if !key.is_empty() && contains_secret(&response, key) {
        return json!({"text":"","finish_reason":null,"usage":{},"model_version":null});
    }
    let candidate = &response["candidates"][0];
    let text = candidate["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p["thought"] != true)
                .filter_map(|p| p["text"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default();
    let usage = response["usageMetadata"]
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .map(|(k, v)| (snake_case(k), v.clone()))
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    json!({"text":text,"finish_reason":candidate["finishReason"],"usage":usage,"model_version":response["modelVersion"]})
}
struct Failure {
    code: Option<u16>,
    kind: &'static str,
    retry_after: Option<String>,
}
async fn detect(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    data: &[u8],
) -> std::result::Result<Value, Failure> {
    let mut credential =
        reqwest::header::HeaderValue::from_bytes(key.as_bytes()).map_err(|_| Failure {
            code: None,
            kind: "TransportError",
            retry_after: None,
        })?;
    credential.set_sensitive(true);
    let response = client
        .post(url)
        .header("x-goog-api-key", credential)
        .json(&request_body(data))
        .send()
        .await
        .map_err(|_| Failure {
            code: None,
            kind: "TransportError",
            retry_after: None,
        })?;
    let code = response.status().as_u16();
    if !response.status().is_success() {
        return Err(Failure {
            code: Some(code),
            kind: "HttpError",
            retry_after: response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Failure {
            code: None,
            kind: "TransportError",
            retry_after: None,
        })?;
        if body.len() + chunk.len() > 16 * 1024 * 1024 {
            return Ok(normalize_response(Value::Null, key));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(normalize_response(
        serde_json::from_slice(&body).unwrap_or(Value::Null),
        key,
    ))
}

pub async fn annotate(
    job_path: &Path,
    key: Option<&str>,
    options: &AnnotateOptions,
) -> Result<Value> {
    annotate_with(job_path, key, options, &RealSleep, &|message| {
        println!("{message}")
    })
    .await
}
/// Injectable waiting/logging preserves journal behavior while tests use local
/// HTTP and immediate, recorded sleeps rather than making paid API calls.
pub async fn annotate_with(
    job_path: &Path,
    key: Option<&str>,
    options: &AnnotateOptions,
    sleeper: &dyn Sleeper,
    emit: &(dyn Fn(&str) + Send + Sync),
) -> Result<Value> {
    if !options.interval.is_finite()
        || options.interval < 1.
        || Duration::try_from_secs_f64(options.interval).is_err()
        || options.retries > 5
    {
        return Err("limit >= 0, interval >= 1 second, retries 0..5 required".into());
    }
    let job_path = fsx::root(job_path)?;
    let _lock = job::lock(&job_path)?;
    let manifest = job::load(&job_path)?;
    job::verify_images(&job_path, &manifest)?;
    let mut pending = Vec::new();
    for entry in &manifest.images {
        let previous = job::read_result(&job_path, entry, &manifest)?;
        let status = previous["status"].as_str().unwrap();
        if status == "pending" || (options.retry_failed && !job::success(status)) {
            pending.push(entry);
        }
    }
    if options.limit != 0 {
        pending.truncate(options.limit);
    }
    emit(&format!(
        "Gemini: model={}, selected={}, interval={}s",
        manifest.model,
        pending.len(),
        options.interval
    ));
    if pending.is_empty() {
        let destination = job::export_unlocked(&job_path, &manifest)?;
        emit(&format!(
            "Nothing to send. Label Studio: {}",
            destination.display()
        ));
        return job::status_unlocked(&job_path, &manifest);
    }
    let key = key
        .filter(|key| !key.is_empty())
        .ok_or("Set GEMINI_API_KEY or use the interactive wizard")?;
    let endpoint = origin(options.endpoint.as_deref())?;
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        // reqwest 0.12.15 retries certain HTTP/2 errors internally. HTTP/1
        // excludes that path, and disabling idle pooling excludes hyper's
        // retry of an unstarted request on a reused connection. All submitted
        // request retries therefore have their own persisted attempt record.
        .http1_only()
        .pool_max_idle_per_host(0);
    // Fixture requests must remain on loopback even when the host environment
    // has a system HTTP proxy configured. Production keeps normal proxy support.
    let builder = if options.endpoint.is_some() {
        builder.no_proxy()
    } else {
        builder
    };
    let client = builder
        .build()
        .map_err(|_| "cannot initialize Gemini HTTP client")?;
    let url = format!(
        "{endpoint}/v1beta/models/{}:generateContent",
        manifest.model
    );
    let mut calls = 0usize;
    let operation: Result<()> = async {
        for (index,entry) in pending.iter().enumerate() {
            let data = std::fs::read(fsx::safe(&job_path,&entry.image)?).map_err(|e|e.to_string())?;
            if hex::encode(sha2::Sha256::digest(&data))!=entry.sha256 { return Err("Image changed during processing".into()); }
            let mut fatal = false;
            for attempt in 0..=options.retries {
                if calls!=0 { sleeper.sleep(Duration::from_secs_f64(options.interval)).await; }
                let mut record = json!({"id":entry.id,"image_sha256":entry.sha256,"policy_hash":manifest.policy_hash,"model":manifest.model,"started_at":job::utc_now(),"status":"unknown"});
                let journal = format!("attempts/{}/{}.json",entry.id,job::unique_stamp()?); let latest = format!("results/{}.json",entry.id);
                fsx::write_json(&job_path,&journal,&record)?; fsx::write_json(&job_path,&latest,&record)?;
                calls += 1; let mut retry = false; let mut delay = Duration::ZERO;
                match detect(&client,&url,key,&data).await {
                    Err(error) => {
                        record["status"] = json!(if error.code.is_some() {"api_error"} else {"unknown"}); record["error_type"] = json!(error.kind); record["http_code"] = json!(error.code);
                        retry = matches!(error.code,Some(429|500|502|503|504)) && attempt<options.retries;
                        fatal = matches!(error.code,Some(400|401|403|404)) || (error.code==Some(429) && !retry);
                        if retry { delay = retry_delay(error.retry_after.as_deref(),attempt+1,SystemTime::now()); }
                    }
                    Ok(raw) => {
                        let boxes = raw["text"].as_str().and_then(|text|serde_json::from_str::<Value>(text).ok());
                        let valid = raw["finish_reason"]=="STOP" && boxes.as_ref().is_some_and(|boxes|job::validate_boxes(boxes).is_ok());
                        if valid {
                            let boxes = boxes.unwrap(); record["status"] = json!(if boxes.as_array().unwrap().is_empty() {"no_detection"} else {"detected"}); record["boxes"] = boxes;
                        } else { record["status"] = json!("invalid_response"); record["error_type"] = json!("InvalidResponse"); }
                        record["response"] = raw;
                    }
                }
                record["finished_at"] = json!(job::utc_now()); fsx::write_json(&job_path,&journal,&record)?; fsx::write_json(&job_path,&latest,&record)?;
                emit(&format!("[{}/{}] {}: {}",index+1,pending.len(),entry.id,record["status"].as_str().unwrap()));
                if retry { emit(&format!("Temporary API error; retry in at least {:.1}s",delay.as_secs_f64())); sleeper.sleep(delay).await; continue; }
                break;
            }
            if fatal { emit("Stopped on API configuration/quota error. Fix it before --retry-failed."); break; }
        }
        Ok(())
    }.await;
    // Export even when local processing fails; unknown records survive process
    // interruption. The CLI also exports after Ctrl+C cancels this future.
    let destination = job::export_unlocked(&job_path, &manifest)?;
    emit(&format!("Label Studio: {}", destination.display()));
    operation?;
    job::status_unlocked(&job_path, &manifest)
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaped_credentials_cannot_survive_response_normalization() {
        let key = "secret\"with\\escapes";
        let response = json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":format!("echo {key}")}]}}]});
        assert_eq!(normalize_response(response, key)["text"], "");
        assert!(contains_secret(&json!({key: [1]}), key));
    }
}
