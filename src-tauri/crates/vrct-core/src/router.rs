use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Mutex as StdMutex;

use tokio::sync::Mutex;

use crate::protocol::{decode_payload, Response};

/// Status code plus result, what the Python handlers returned as
/// `{"status": .., "result": ..}`.
pub type Reply = (u16, Value);

type HandlerFn = Arc<dyn Fn(Option<Value>) -> BoxFuture<'static, Reply> + Send + Sync>;
type Gate = Arc<dyn Fn() -> bool + Send + Sync>;

struct Entry {
    handler: HandlerFn,
    /// `None` means always served by Rust. With a gate, Rust serves only while
    /// it returns true and the sidecar answers otherwise.
    gate: Option<Gate>,
}

impl Entry {
    fn serves(&self) -> bool {
        self.gate.as_ref().is_none_or(|gate| gate())
    }
}

/// Where finished responses go (the UI event channel in production).
pub trait ResponseSink: Send + Sync {
    fn emit(&self, response: Response);
}

/// Receives requests for endpoints not yet ported to Rust.
pub trait Fallback: Send + Sync {
    /// `data` is the UI's base64 payload, forwarded untouched.
    fn forward(&self, endpoint: &str, data: Option<&str>) -> Result<(), String>;
}

/// Same wait ceiling as the Python worker (0.05 s x 400 retries).
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Router {
    handlers: HashMap<String, Entry>,
    /// Gated requests handed to the sidecar, per endpoint, whose replies are
    /// still due. Without this the reply would be dropped as stale if the gate
    /// opened in the meantime.
    forwarded: StdMutex<HashMap<String, usize>>,
    locks: HashMap<String, Arc<Mutex<()>>>,
    fallback: Option<Arc<dyn Fallback>>,
    sink: Arc<dyn ResponseSink>,
    lock_timeout: Duration,
}

/// `/set/enable/x` and `/set/disable/x` share one lock, like in `mainloop.py`.
fn canonical_lock_key(endpoint: &str) -> String {
    for prefix in ["/set/enable/", "/set/disable/"] {
        if let Some(rest) = endpoint.strip_prefix(prefix) {
            return format!("/lock/set/{rest}");
        }
    }
    endpoint.to_string()
}

impl Router {
    pub fn new(sink: Arc<dyn ResponseSink>) -> Self {
        Self {
            handlers: HashMap::new(),
            forwarded: StdMutex::default(),
            locks: HashMap::new(),
            fallback: None,
            sink,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn with_fallback(mut self, fallback: Arc<dyn Fallback>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// Register a Rust implementation of `endpoint`.
    pub fn handle<F, Fut>(self, endpoint: &str, handler: F) -> Self
    where
        F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        self.register(endpoint, None, handler)
    }

    /// Like `handle`, but Rust only serves the endpoint while `gate()` holds;
    /// until then the request goes to the sidecar.
    pub fn handle_when<G, F, Fut>(self, endpoint: &str, gate: G, handler: F) -> Self
    where
        G: Fn() -> bool + Send + Sync + 'static,
        F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        self.register(endpoint, Some(Arc::new(gate)), handler)
    }

    fn register<F, Fut>(mut self, endpoint: &str, gate: Option<Gate>, handler: F) -> Self
    where
        F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        self.locks
            .entry(canonical_lock_key(endpoint))
            .or_insert_with(|| Arc::new(Mutex::new(())));
        self.handlers.insert(
            endpoint.to_string(),
            Entry {
                handler: Arc::new(move |data| Box::pin(handler(data))),
                gate,
            },
        );
        self
    }

    /// True when Rust serves this endpoint right now, so the sidecar must not.
    pub fn is_owned(&self, endpoint: &str) -> bool {
        self.handlers.get(endpoint).is_some_and(Entry::serves)
    }

    /// Pass a sidecar-originated response to the UI unless Rust owns the
    /// endpoint (the sidecar's version would be stale or wrong).
    pub fn accept_sidecar_response(&self, response: Response) {
        if !self.is_owned(&response.endpoint) || self.take_forwarded(&response.endpoint) {
            self.sink.emit(response);
        }
    }

    fn take_forwarded(&self, endpoint: &str) -> bool {
        let mut forwarded = self.forwarded.lock().unwrap();
        match forwarded.get_mut(endpoint) {
            Some(count) if *count > 0 => {
                *count -= 1;
                true
            }
            _ => false,
        }
    }

    /// Fire-and-forget, like writing a line to the sidecar's stdin. The
    /// result arrives later through the sink.
    pub fn dispatch(self: &Arc<Self>, endpoint: String, data: Option<String>) {
        let entry = self.handlers.get(&endpoint);
        let Some(entry) = entry.filter(|entry| entry.serves()) else {
            let gated = self.handlers.contains_key(&endpoint);
            if self.forward_or_404(endpoint.clone(), data) && gated {
                *self.forwarded.lock().unwrap().entry(endpoint).or_default() += 1;
            }
            return;
        };
        let handler = Arc::clone(&entry.handler);
        let router = Arc::clone(self);
        tokio::spawn(async move {
            let response = router.run_handler(&endpoint, handler, data).await;
            router.sink.emit(response);
        });
    }

    /// True when the request reached the sidecar.
    fn forward_or_404(&self, endpoint: String, data: Option<String>) -> bool {
        let outcome = match &self.fallback {
            Some(fallback) => fallback.forward(&endpoint, data.as_deref()),
            None => Err("Invalid endpoint".to_string()),
        };
        match outcome {
            Ok(()) => true,
            Err(message) => {
                let status = if self.fallback.is_some() { 503 } else { 404 };
                self.sink.emit(Response::new(status, endpoint, json!(message)));
                false
            }
        }
    }

    async fn run_handler(
        &self,
        endpoint: &str,
        handler: HandlerFn,
        data: Option<String>,
    ) -> Response {
        let payload = match data.as_deref().map(decode_payload).transpose() {
            Ok(payload) => payload,
            Err(_) => return Response::new(400, endpoint, json!("Invalid payload")),
        };

        let lock = self.locks.get(&canonical_lock_key(endpoint)).cloned();
        let _guard = match lock {
            Some(lock) => {
                match tokio::time::timeout(self.lock_timeout, lock.lock_owned()).await {
                    Ok(guard) => Some(guard),
                    Err(_) => {
                        return Response::new(423, endpoint, json!("Locked endpoint (busy)"))
                    }
                }
            }
            None => None,
        };

        // Own task so a panicking handler becomes a 500 instead of killing
        // the dispatcher.
        match tokio::spawn(handler(payload)).await {
            Ok((status, result)) => Response::new(status, endpoint, result),
            Err(_) => Response::new(500, endpoint, json!("Internal error")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Recorder {
        responses: StdMutex<Vec<Response>>,
        notify: Notify,
    }

    impl ResponseSink for Recorder {
        fn emit(&self, response: Response) {
            self.responses.lock().unwrap().push(response);
            self.notify.notify_waiters();
        }
    }

    impl Recorder {
        async fn next(&self, count: usize) -> Vec<Response> {
            loop {
                let notified = self.notify.notified();
                {
                    let responses = self.responses.lock().unwrap();
                    if responses.len() >= count {
                        return responses.clone();
                    }
                }
                notified.await;
            }
        }
    }

    #[derive(Default)]
    struct FakeFallback {
        calls: StdMutex<Vec<(String, Option<String>)>>,
    }

    impl Fallback for FakeFallback {
        fn forward(&self, endpoint: &str, data: Option<&str>) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((endpoint.to_string(), data.map(str::to_string)));
            Ok(())
        }
    }

    fn encode(value: &Value) -> String {
        STANDARD.encode(value.to_string())
    }

    #[test]
    fn enable_and_disable_share_a_lock_key() {
        assert_eq!(
            canonical_lock_key("/set/enable/translation"),
            canonical_lock_key("/set/disable/translation")
        );
        assert_eq!(canonical_lock_key("/run/x"), "/run/x");
    }

    #[tokio::test]
    async fn owned_endpoint_runs_handler_with_decoded_payload() {
        let sink = Arc::new(Recorder::default());
        let router = Arc::new(Router::new(sink.clone()).handle("/echo", |data| async move {
            (200, data.unwrap_or(Value::Null))
        }));

        router.dispatch("/echo".into(), Some(encode(&json!({"a": 1}))));

        let responses = sink.next(1).await;
        assert_eq!(responses[0], Response::new(200, "/echo", json!({"a": 1})));
    }

    #[tokio::test]
    async fn unknown_endpoint_goes_to_fallback_untouched() {
        let sink = Arc::new(Recorder::default());
        let fallback = Arc::new(FakeFallback::default());
        let router = Arc::new(Router::new(sink.clone()).with_fallback(fallback.clone()));

        router.dispatch("/get/data/version".into(), Some("QUJD".into()));

        let calls = fallback.calls.lock().unwrap();
        assert_eq!(
            calls.as_slice(),
            [("/get/data/version".to_string(), Some("QUJD".to_string()))]
        );
        assert!(sink.responses.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_endpoint_without_fallback_is_404() {
        let sink = Arc::new(Recorder::default());
        let router = Arc::new(Router::new(sink.clone()));
        router.dispatch("/nope".into(), None);
        assert_eq!(sink.next(1).await[0].status, 404);
    }

    #[tokio::test]
    async fn bad_payload_is_400() {
        let sink = Arc::new(Recorder::default());
        let router = Arc::new(Router::new(sink.clone()).handle("/x", |_| async { (200, json!(1)) }));
        router.dispatch("/x".into(), Some("%%%".into()));
        assert_eq!(sink.next(1).await[0].status, 400);
    }

    #[tokio::test]
    async fn panicking_handler_becomes_500() {
        let sink = Arc::new(Recorder::default());
        let router = Arc::new(Router::new(sink.clone()).handle("/boom", |_| async {
            panic!("handler bug");
            #[allow(unreachable_code)]
            (200, Value::Null)
        }));
        router.dispatch("/boom".into(), None);
        assert_eq!(sink.next(1).await[0].status, 500);
    }

    #[tokio::test(start_paused = true)]
    async fn second_request_on_a_busy_endpoint_gets_423_after_timeout() {
        let sink = Arc::new(Recorder::default());
        let router = Arc::new(
            Router::new(sink.clone())
                .with_lock_timeout(Duration::from_secs(5))
                .handle("/slow", |_| async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    (200, json!("done"))
                }),
        );

        router.dispatch("/slow".into(), None);
        tokio::task::yield_now().await;
        router.dispatch("/slow".into(), None);

        let responses = sink.next(1).await;
        assert_eq!(responses[0].status, 423);
        let responses = sink.next(2).await;
        assert_eq!(responses[1].status, 200);
    }

    #[tokio::test]
    async fn sidecar_responses_for_owned_endpoints_are_dropped() {
        let sink = Arc::new(Recorder::default());
        let router = Router::new(sink.clone()).handle("/owned", |_| async { (200, json!(1)) });

        router.accept_sidecar_response(Response::new(200, "/owned", json!("stale")));
        router.accept_sidecar_response(Response::new(200, "/other", json!("kept")));

        let responses = sink.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].endpoint, "/other");
    }

    fn gated_router(sink: Arc<Recorder>, fallback: Arc<FakeFallback>, open: Arc<std::sync::atomic::AtomicBool>) -> Arc<Router> {
        Arc::new(
            Router::new(sink)
                .with_fallback(fallback)
                .handle_when(
                    "/get/data/x",
                    move || open.load(std::sync::atomic::Ordering::SeqCst),
                    |_| async { (200, json!("from rust")) },
                ),
        )
    }

    #[tokio::test]
    async fn closed_gate_sends_the_request_to_the_sidecar() {
        let (sink, fallback) = (Arc::new(Recorder::default()), Arc::new(FakeFallback::default()));
        let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let router = gated_router(sink.clone(), fallback.clone(), open);

        router.dispatch("/get/data/x".into(), None);

        assert_eq!(fallback.calls.lock().unwrap().len(), 1);
        assert!(sink.responses.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_gate_is_served_by_rust_and_the_sidecar_copy_is_dropped() {
        let (sink, fallback) = (Arc::new(Recorder::default()), Arc::new(FakeFallback::default()));
        let open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let router = gated_router(sink.clone(), fallback.clone(), open);

        router.dispatch("/get/data/x".into(), None);
        assert_eq!(sink.next(1).await[0].result, json!("from rust"));
        assert!(fallback.calls.lock().unwrap().is_empty());

        router.accept_sidecar_response(Response::new(200, "/get/data/x", json!("stale")));
        assert_eq!(sink.responses.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reply_to_a_request_forwarded_before_the_gate_opened_still_reaches_the_ui() {
        let (sink, fallback) = (Arc::new(Recorder::default()), Arc::new(FakeFallback::default()));
        let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let router = gated_router(sink.clone(), fallback, open.clone());

        router.dispatch("/get/data/x".into(), None);
        open.store(true, std::sync::atomic::Ordering::SeqCst);

        // The sidecar answers the forwarded request after the gate opened...
        router.accept_sidecar_response(Response::new(200, "/get/data/x", json!("python")));
        // ...but an unsolicited second copy is stale.
        router.accept_sidecar_response(Response::new(200, "/get/data/x", json!("stale")));

        let responses = sink.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].result, json!("python"));
    }

    #[tokio::test]
    async fn failed_forward_is_not_counted_as_a_pending_reply() {
        struct Down;
        impl Fallback for Down {
            fn forward(&self, _: &str, _: Option<&str>) -> Result<(), String> {
                Err("not running".into())
            }
        }
        let sink = Arc::new(Recorder::default());
        let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate = open.clone();
        let router = Arc::new(
            Router::new(sink.clone())
                .with_fallback(Arc::new(Down))
                .handle_when("/get/data/x", move || gate.load(std::sync::atomic::Ordering::SeqCst), |_| async {
                    (200, json!("from rust"))
                }),
        );

        router.dispatch("/get/data/x".into(), None);
        assert_eq!(sink.next(1).await[0].status, 503);

        open.store(true, std::sync::atomic::Ordering::SeqCst);
        router.accept_sidecar_response(Response::new(200, "/get/data/x", json!("stale")));
        assert_eq!(sink.responses.lock().unwrap().len(), 1);
    }
}
