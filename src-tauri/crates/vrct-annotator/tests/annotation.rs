use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vrct_annotator::{
    gemini::{self, AnnotateOptions, Sleeper},
    job,
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("作業 with spaces");
        fs::create_dir(&root).unwrap();
        Self {
            _directory: directory,
            root,
        }
    }
    fn capture(&self, session: &str, name: &str) -> PathBuf {
        let folder = self.root.join("input").join(session).join("unlabeled");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{name}.png"));
        image::RgbImage::from_pixel(200, 100, image::Rgb([0, 0, 100]))
            .save(&path)
            .unwrap();
        write_json(
            &path.with_extension("json"),
            &json!({"image":format!("{name}.png"),"width":200,"height":100,"label":"unlabeled","session":session,"run_id":session,"backend":"openvr_d3d11"}),
        );
        path
    }
    fn prepared(count: usize) -> (Self, PathBuf, job::Manifest) {
        let fixture = Self::new();
        for i in 0..count {
            fixture.capture("日本語", &i.to_string());
        }
        let path = fixture.root.join("job");
        let manifest = job::prepare(
            &fixture.root.join("input"),
            &path,
            0,
            42,
            job::DEFAULT_MODEL,
        )
        .unwrap();
        (fixture, path, manifest)
    }
}
fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn box2d() -> Value {
    json!({"box_2d":[100,200,400,600],"label":"chat_box"})
}
fn api(text: &str, finish: &str) -> Value {
    json!({"candidates":[{"content":{"role":"model","parts":[{"text":text}]},"finishReason":finish}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5},"modelVersion":job::DEFAULT_MODEL})
}
fn response(boxes: Value) -> Value {
    api(&serde_json::to_string(&boxes).unwrap(), "STOP")
}
enum Action {
    Reply(u16, Value, &'static str),
    Hang,
    Disconnect,
}
struct Server {
    origin: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new(actions: Vec<Action>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let mut actions = VecDeque::from(actions);
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let (header_end, length) =
                    loop {
                        let count = socket.read(&mut buffer).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&bytes[..end]);
                            let length = header
                                .lines()
                                .find_map(|line| {
                                    line.split_once(':')
                                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap();
                            assert!(header.lines().next().unwrap().starts_with(
                                "POST /v1beta/models/gemini-2.5-flash:generateContent "
                            ));
                            assert!(header
                                .to_ascii_lowercase()
                                .contains("x-goog-api-key: sensitive-fake-key"));
                            break (end + 4, length);
                        }
                    };
                while bytes.len() < header_end + length {
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap());
                match actions.pop_front().expect("unexpected API call") {
                    Action::Hang => {
                        std::future::pending::<()>().await;
                    }
                    Action::Disconnect => {
                        drop(socket);
                    }
                    Action::Reply(code, value, headers) => {
                        let body = serde_json::to_vec(&value).unwrap();
                        let head=format!("HTTP/1.1 {code} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",body.len());
                        socket.write_all(head.as_bytes()).await.unwrap();
                        socket.write_all(&body).await.unwrap();
                    }
                }
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Default)]
struct Immediate(Arc<Mutex<Vec<Duration>>>);
impl Sleeper for Immediate {
    fn sleep(
        &self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.0.lock().unwrap().push(duration);
        Box::pin(std::future::ready(()))
    }
}
async fn run(
    path: &Path,
    server: &Server,
    retries: u8,
    retry_failed: bool,
    sleeper: &Immediate,
) -> Value {
    gemini::annotate_with(
        path,
        Some("sensitive-fake-key"),
        &AnnotateOptions {
            endpoint: Some(server.origin.clone()),
            retries,
            retry_failed,
            limit: 0,
            ..Default::default()
        },
        sleeper,
        &|_| {},
    )
    .await
    .unwrap()
}
fn json_files(root: &Path, results: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            json_files(&entry.path(), results);
        } else if entry.path().extension().is_some_and(|e| e == "json") {
            results.push(entry.path());
        }
    }
}

#[test]
fn policy_hash_matches_legacy_python_serialization() {
    assert_eq!(
        job::policy_hash(job::DEFAULT_MODEL),
        "282c1e0524ab918bf33c4ffbae3b1a1d9cfa10fbe1938c98412715b339957522"
    );
}

#[test]
fn snapshots_preserve_sources_same_names_and_seeded_run_coverage() {
    let fixture = Fixture::new();
    let mut source = Vec::new();
    for session in ["A", "B", "C"] {
        for index in 0..5 {
            let path = fixture.capture(session, &index.to_string());
            source.push((path.clone(), fs::read(path).unwrap()));
        }
    }
    let a = job::prepare(
        &fixture.root.join("input"),
        &fixture.root.join("a"),
        3,
        42,
        job::DEFAULT_MODEL,
    )
    .unwrap();
    let b = job::prepare(
        &fixture.root.join("input"),
        &fixture.root.join("b"),
        3,
        42,
        job::DEFAULT_MODEL,
    )
    .unwrap();
    assert_eq!(
        a.images.iter().map(|e| &e.id).collect::<Vec<_>>(),
        b.images.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
    assert_eq!(
        a.images
            .iter()
            .map(|e| e.capture["session"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        ["A", "B", "C"].into_iter().collect()
    );
    for (path, before) in source {
        assert_eq!(fs::read(path).unwrap(), before);
    }
    assert!(job::prepare(
        &fixture.root.join("input"),
        &fixture.root.join("a"),
        0,
        42,
        job::DEFAULT_MODEL
    )
    .unwrap_err()
    .contains("NEW"));
    assert!(job::prepare(
        &fixture.root.join("input"),
        &fixture.root.join("input/job"),
        0,
        42,
        job::DEFAULT_MODEL
    )
    .unwrap_err()
    .contains("outside"));
}

#[test]
fn invalid_capture_never_publishes_a_partial_job() {
    for damage in [
        "missing",
        "dimensions",
        "invalid_png",
        "image_name",
        "float_dimension",
    ] {
        let fixture = Fixture::new();
        let path = fixture.capture("A", "same");
        let metadata = path.with_extension("json");
        match damage {
            "missing" => fs::remove_file(&metadata).unwrap(),
            "invalid_png" => fs::write(&path, b"not png").unwrap(),
            _ => {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
                match damage {
                    "dimensions" => value["width"] = json!(123),
                    "float_dimension" => value["width"] = json!(200.0),
                    _ => value["image"] = json!("other.png"),
                };
                write_json(&metadata, &value);
            }
        }
        assert!(
            job::prepare(
                &fixture.root.join("input"),
                &fixture.root.join("job"),
                0,
                42,
                job::DEFAULT_MODEL
            )
            .is_err(),
            "accepted {damage}"
        );
        assert!(!fixture.root.join("job").exists());
        assert!(!fs::read_dir(&fixture.root).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".prepare_")));
    }
}

#[test]
fn rotated_png_is_rejected_without_a_partial_job() {
    use image::ImageEncoder;
    let fixture = Fixture::new();
    let path = fixture.capture("session", "rotated");
    let mut encoder = image::codecs::png::PngEncoder::new(fs::File::create(path).unwrap());
    encoder
        .set_exif_metadata(vec![
            73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ])
        .unwrap();
    encoder
        .write_image(
            &vec![0; 200 * 100 * 3],
            200,
            100,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let destination = fixture.root.join("job");
    assert!(job::prepare(
        &fixture.root.join("input"),
        &destination,
        0,
        42,
        job::DEFAULT_MODEL
    )
    .unwrap_err()
    .contains("Rotated image"));
    assert!(!destination.exists());
}

#[test]
fn rejects_all_malformed_box_shapes_without_partial_acceptance() {
    for value in [
        Value::Null,
        json!({}),
        json!([{"box_2d":[1,2,1,4],"label":"chat_box"}]),
        json!([{"box_2d":[-1,0,500,500],"label":"chat_box"}]),
        json!([{"box_2d":[0,0,1001,1000],"label":"chat_box"}]),
        json!([{"box_2d":[800,900,100,200],"label":"chat_box"}]),
        json!([{"box_2d":[true,0,100,100],"label":"chat_box"}]),
        json!([{"box_2d":[0.0,0,100,100],"label":"chat_box"}]),
        json!([{"box_2d":[0,1,2],"label":"chat_box"}]),
        json!([{"box_2d":[0,0,2,2],"label":"nameplate"}]),
        json!([box2d(), box2d()]),
        json!([box2d(),{"unexpected":1}]),
    ] {
        assert!(job::validate_boxes(&value).is_err(), "accepted {value}");
    }
    job::validate_boxes(&json!([])).unwrap();
    job::validate_boxes(&json!([{"box_2d":[0,0,1000,1000],"label":"chat_box"}])).unwrap();
}

#[tokio::test]
async fn actual_rest_payload_multibox_empty_invalid_predictions_and_usage() {
    let (_fixture, path, manifest) = Fixture::prepared(3);
    let server = Server::new(vec![
        Action::Reply(
            200,
            response(json!([box2d(),{"box_2d":[500,100,900,300],"label":"chat_box"}])),
            "",
        ),
        Action::Reply(200, response(json!([])), ""),
        Action::Reply(200, api("```json\n[]\n```", "STOP"), ""),
    ])
    .await;
    let summary = run(&path, &server, 2, false, &Immediate::default()).await;
    assert_eq!(
        summary["counts"],
        json!({"detected":1,"no_detection":1,"invalid_response":1})
    );
    assert_eq!(summary["reported_usage"]["prompt_token_count"], 30);
    let request = &server.requests.lock().unwrap()[0];
    assert_eq!(
        request["generationConfig"]["responseJsonSchema"],
        job::schema()
    );
    assert_eq!(
        request["contents"][0]["parts"][0]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(request["generationConfig"]["temperature"], 0);
    assert_eq!(request["generationConfig"]["maxOutputTokens"], 8192);
    assert_eq!(request["contents"][0]["parts"][1]["text"], job::PROMPT);
    let exported = job::export(&path).unwrap();
    let tasks: Value =
        serde_json::from_slice(&fs::read(exported.join("tasks.json")).unwrap()).unwrap();
    assert!(tasks
        .as_array()
        .unwrap()
        .iter()
        .all(|task| task.get("annotations").is_none()));
    let regions = &tasks[0]["predictions"][0]["result"];
    assert_eq!(regions.as_array().unwrap().len(), 2);
    assert_eq!(
        regions[0]["value"],
        json!({"x":20.0,"y":10.0,"width":40.0,"height":30.0,"rotation":0,"rectanglelabels":["chat_box"]})
    );
    assert_eq!(regions[0]["original_width"], 200);
    assert_eq!(regions[0]["original_height"], 100);
    assert_eq!(tasks[1]["predictions"][0]["result"], json!([]));
    assert!(tasks[2].get("predictions").is_none());
    assert_eq!(tasks.as_array().unwrap().len(), manifest.images.len());
}

#[tokio::test]
async fn resume_skips_success_and_requires_explicit_failed_retry() {
    let (_fixture, path, _) = Fixture::prepared(2);
    let server = Server::new(vec![
        Action::Reply(200, response(json!([box2d()])), ""),
        Action::Reply(200, api("invalid", "STOP"), ""),
        Action::Reply(200, response(json!([])), ""),
    ])
    .await;
    run(&path, &server, 0, false, &Immediate::default()).await;
    let summary = run(&path, &server, 0, false, &Immediate::default()).await;
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(summary["counts"]["invalid_response"], 1);
    let summary = run(&path, &server, 0, true, &Immediate::default()).await;
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    assert_eq!(summary["counts"], json!({"detected":1,"no_detection":1}));
    assert_eq!(summary["reported_usage"]["prompt_token_count"], 30);
}

#[tokio::test]
async fn retry_after_and_interval_are_journalled_and_keys_never_persist() {
    let (_fixture, path, _) = Fixture::prepared(2);
    let server = Server::new(vec![
        Action::Reply(
            429,
            json!({"error":"sensitive-fake-key"}),
            "Retry-After: 7\r\n",
        ),
        Action::Reply(200, response(json!([box2d()])), ""),
        Action::Reply(200, response(json!([box2d()])), ""),
    ])
    .await;
    let sleeper = Immediate::default();
    let summary = run(&path, &server, 2, false, &sleeper).await;
    assert_eq!(summary["counts"], json!({"detected":2}));
    assert_eq!(
        *sleeper.0.lock().unwrap(),
        vec![
            Duration::from_secs(7),
            Duration::from_secs(6),
            Duration::from_secs(6)
        ]
    );
    let mut files = Vec::new();
    json_files(&path, &mut files);
    assert!(files.iter().all(|p| !fs::read_to_string(p)
        .unwrap()
        .contains("sensitive-fake-key")));
}

#[tokio::test]
async fn configuration_and_exhausted_quota_errors_stop_remaining_images() {
    for code in [400, 401, 403, 404, 429] {
        let (_fixture, path, _) = Fixture::prepared(2);
        let server = Server::new(vec![Action::Reply(
            code,
            json!({"error":"sensitive-fake-key"}),
            "",
        )])
        .await;
        let summary = run(&path, &server, 0, false, &Immediate::default()).await;
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        assert_eq!(summary["counts"], json!({"api_error":1,"pending":1}));
    }
}

#[tokio::test]
async fn blocked_incomplete_and_credential_echo_responses_are_invalid() {
    for body in [
        api("[]", "MAX_TOKENS"),
        api("[]", "SAFETY"),
        api("[{}]", "STOP"),
        api("sensitive-fake-key", "STOP"),
        json!({"promptFeedback":{"blockReason":"SAFETY"}}),
    ] {
        let (_fixture, path, _) = Fixture::prepared(1);
        let server = Server::new(vec![Action::Reply(200, body, "")]).await;
        let summary = run(&path, &server, 0, false, &Immediate::default()).await;
        assert_eq!(summary["counts"], json!({"invalid_response":1}));
        let mut files = Vec::new();
        json_files(&path, &mut files);
        assert!(files.iter().all(|p| !fs::read_to_string(p)
            .unwrap()
            .contains("sensitive-fake-key")));
    }
}

#[tokio::test]
async fn image_and_policy_mutation_stop_before_http() {
    let (_fixture, path, mut manifest) = Fixture::prepared(1);
    fs::write(path.join(&manifest.images[0].image), b"changed").unwrap();
    assert!(gemini::annotate(
        &path,
        Some("sensitive-fake-key"),
        &AnnotateOptions::default()
    )
    .await
    .unwrap_err()
    .contains("changed"));
    assert!(!path.join("attempts").exists());
    manifest.model = "gemini-something-else".into();
    write_json(
        &path.join("manifest.json"),
        &serde_json::to_value(manifest).unwrap(),
    );
    assert!(job::load(&path).unwrap_err().contains("Model/prompt"));
}

#[tokio::test]
async fn cancelled_inflight_call_remains_unknown_and_is_not_automatically_resent() {
    let (_fixture, path, manifest) = Fixture::prepared(1);
    let server = Server::new(vec![Action::Hang]).await;
    let owned = path.clone();
    let origin = server.origin.clone();
    let task = tokio::spawn(async move {
        gemini::annotate_with(
            &owned,
            Some("sensitive-fake-key"),
            &AnnotateOptions {
                endpoint: Some(origin),
                ..Default::default()
            },
            &Immediate::default(),
            &|_| {},
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        job::read_result(&path, &manifest.images[0], &manifest).unwrap()["status"],
        "unknown"
    );
    let guard = job::lock(&path).unwrap();
    drop(guard);
    let summary = gemini::annotate(&path, None, &AnnotateOptions::default())
        .await
        .unwrap();
    assert_eq!(summary["counts"], json!({"unknown":1}));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn transport_disconnect_does_not_hide_a_second_billable_request() {
    let (_fixture, path, manifest) = Fixture::prepared(1);
    let server = Server::new(vec![Action::Disconnect]).await;
    let sleeper = Immediate::default();
    let summary = run(&path, &server, 5, false, &sleeper).await;
    assert_eq!(summary["counts"]["unknown"], 1);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(sleeper.0.lock().unwrap().is_empty());
    let attempts = fs::read_dir(path.join("attempts").join(&manifest.images[0].id))
        .unwrap()
        .count();
    assert_eq!(attempts, 1);
    let resumed = run(&path, &server, 5, false, &sleeper).await;
    assert_eq!(resumed["counts"]["unknown"], 1);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn lock_releases_after_scope_and_rejects_concurrent_access() {
    let (_fixture, path, _) = Fixture::prepared(1);
    {
        let _guard = job::lock(&path).unwrap();
        assert!(job::lock(&path).is_err());
    }
    drop(job::lock(&path).unwrap());
}

#[test]
fn exports_are_additive_and_leave_human_work_untouched() {
    let (_fixture, path, _) = Fixture::prepared(1);
    fs::write(path.join("human_annotations.json"), b"leave unchanged").unwrap();
    let a = job::export(&path).unwrap();
    let before = fs::read(a.join("tasks.json")).unwrap();
    let b = job::export(&path).unwrap();
    assert_ne!(a, b);
    assert_eq!(fs::read(a.join("tasks.json")).unwrap(), before);
    assert_eq!(
        fs::read(path.join("human_annotations.json")).unwrap(),
        b"leave unchanged"
    );
    assert!(!path.join("labels").exists());
}

#[test]
fn malicious_manifest_paths_and_mismatched_result_identity_are_rejected() {
    let (_fixture, path, manifest) = Fixture::prepared(1);
    let mut altered = serde_json::to_value(&manifest).unwrap();
    altered["images"][0]["image"] = json!("../outside.png");
    write_json(&path.join("manifest.json"), &altered);
    assert!(job::load(&path).is_err());
    write_json(
        &path.join("manifest.json"),
        &serde_json::to_value(&manifest).unwrap(),
    );
    write_json(
        &path
            .join("results")
            .join(format!("{}.json", manifest.images[0].id)),
        &json!({"status":"detected","boxes":[box2d()],"image_sha256":"wrong","policy_hash":manifest.policy_hash}),
    );
    assert!(job::status(&path).is_err());
}

#[test]
fn retry_after_dates_numbers_and_malformed_headers() {
    let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let date = httpdate::fmt_http_date(now + Duration::from_secs(7));
    assert_eq!(
        gemini::retry_delay(Some(&date), 1, now),
        Duration::from_secs(7)
    );
    assert_eq!(
        gemini::retry_delay(Some("7"), 1, now),
        Duration::from_secs(7)
    );
    for header in ["NaN", "inf", "-1", "nonsense"] {
        let delay = gemini::retry_delay(Some(header), 1, now);
        assert!((2.0..=3.0).contains(&delay.as_secs_f64()));
    }
}

#[tokio::test]
async fn invalid_options_and_remote_endpoint_override_cannot_send_credentials() {
    let (_fixture, path, _) = Fixture::prepared(1);
    for options in [
        AnnotateOptions {
            interval: f64::NAN,
            ..Default::default()
        },
        AnnotateOptions {
            interval: 0.,
            ..Default::default()
        },
        AnnotateOptions {
            retries: 6,
            ..Default::default()
        },
        AnnotateOptions {
            endpoint: Some("https://example.com".into()),
            ..Default::default()
        },
    ] {
        assert!(
            gemini::annotate(&path, Some("sensitive-fake-key"), &options)
                .await
                .is_err()
        );
    }
    assert!(!path.join("attempts").exists());
}

#[test]
fn cli_check_noninteractive_and_offline_prepare_status_export_need_no_key() {
    let fixture = Fixture::new();
    fixture.capture("日本語", "same");
    let binary = env!("CARGO_BIN_EXE_vrct-annotator");
    let run = |args: Vec<std::ffi::OsString>| {
        std::process::Command::new(binary)
            .args(args)
            .env_remove("GEMINI_API_KEY")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    assert!(run(vec!["check".into()]).status.success());
    assert!(!run(Vec::new()).status.success());
    let path = fixture.root.join("job");
    assert!(run(vec![
        "prepare".into(),
        fixture.root.join("input").into_os_string(),
        "--out".into(),
        path.clone().into_os_string(),
        "--limit".into(),
        "0".into(),
        "--seed".into(),
        "-42".into()
    ])
    .status
    .success());
    for command in ["status", "export"] {
        assert!(run(vec![command.into(), path.clone().into_os_string()])
            .status
            .success());
    }
    assert!(!path.join("attempts").exists());
}
