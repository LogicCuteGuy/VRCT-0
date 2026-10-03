use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde_json::{json, Value};
use vrct_core::osc_query::{
    decode_mute_packet, OscQueryService, MUTE_ADDRESS, OSC_SERVICE, QUERY_SERVICE,
};

fn mute_packet(value: bool) -> Vec<u8> {
    let mut packet = MUTE_ADDRESS.as_bytes().to_vec();
    packet.push(0);
    while !packet.len().is_multiple_of(4) {
        packet.push(0);
    }
    packet.extend_from_slice(if value { b",T\0\0" } else { b",F\0\0" });
    packet
}

fn bundle(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut packet = b"#bundle\0\0\0\0\0\0\0\0\x01".to_vec();
    for part in parts {
        packet.extend_from_slice(&(part.len() as u32).to_be_bytes());
        packet.extend_from_slice(part);
    }
    packet
}

fn test_port() -> u16 {
    // mdns-sd's Windows per-interface socket path binds MDNS_PORT even when
    // new_with_port receives a custom value. Exercise the real production
    // path there; Unix has working custom-port packet-info sockets.
    if cfg!(windows) {
        return 5353;
    }
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fetch(endpoint: std::net::SocketAddr, path: &str) -> Value {
    let mut stream = TcpStream::connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    serde_json::from_str(body.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[test]
fn parser_rejects_truncated_malformed_and_deep_bundles_atomically() {
    assert_eq!(decode_mute_packet(&mute_packet(true)), Some(vec![true]));
    assert_eq!(
        decode_mute_packet(&bundle(&[mute_packet(true), bundle(&[mute_packet(false)])])),
        Some(vec![true, false])
    );
    let valid = mute_packet(true);
    for end in 0..valid.len() {
        assert!(decode_mute_packet(&valid[..end]).is_none());
    }
    assert!(decode_mute_packet(&bundle(&[valid.clone(), b"bad!".to_vec()])).is_none());
    let mut wrong_type = valid.clone();
    let len = wrong_type.len();
    wrong_type[len - 3] = b'i';
    assert!(decode_mute_packet(&wrong_type).is_none());
    let mut deep = valid;
    for _ in 0..18 {
        deep = bundle(&[deep]);
    }
    assert!(decode_mute_packet(&deep).is_none());
    for bytes in [vec![0; 4], vec![255; 4096], vec![b'/'; 64]] {
        assert!(decode_mute_packet(&bytes).is_none());
    }
}

#[test]
fn udp_http_duplicate_events_and_shutdown_release_sockets() {
    let (sender, events) = mpsc::channel();
    // This test owns only UDP input. Never let a live VRChat client or delayed
    // goodbye/query events from the discovery test change its mute state.
    let peer_prefix = format!("VRCT-udp-test-peer-{}", std::process::id());
    let service = OscQueryService::with_discovery_prefix(
        Arc::new(move |value| {
            sender.send(value).unwrap();
        }),
        test_port(),
        &peer_prefix,
    );
    assert!(!service.query_enabled());
    service.start().unwrap();
    service.start().unwrap();
    assert!(service.query_enabled());
    let endpoints = service.endpoints().unwrap();
    let host = fetch(endpoints.http, "/?HOST_INFO");
    assert_eq!(host["OSC_PORT"], endpoints.osc.port());
    assert!(host["NAME"].as_str().unwrap().starts_with("VRCT-"));
    let tree = fetch(endpoints.http, "/");
    assert_eq!(
        tree["CONTENTS"]["avatar"]["CONTENTS"]["parameters"]["CONTENTS"]["MuteSelf"]["ACCESS"],
        3
    );
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .send_to(
            &bundle(&[mute_packet(true), mute_packet(true)]),
            endpoints.osc,
        )
        .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(3)).unwrap(),
        Some(true)
    );
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
    socket.send_to(b"garbage", endpoints.osc).unwrap();
    socket
        .send_to(
            &bundle(&[mute_packet(false), b"bad!".to_vec()]),
            endpoints.osc,
        )
        .unwrap();
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(service.current_mute(), Some(true));
    assert_eq!(fetch(endpoints.http, MUTE_ADDRESS)["VALUE"], json!([true]));
    let mut slow_client = TcpStream::connect(endpoints.http).unwrap();
    slow_client.write_all(b"GET / HTTP/1.1\r\n").unwrap();
    let stop_time = Instant::now();
    service.stop();
    assert!(!service.query_enabled());
    assert!(stop_time.elapsed() < Duration::from_secs(3));
    assert_eq!(events.recv_timeout(Duration::from_secs(1)).unwrap(), None);
    assert!(service.endpoints().is_none());
    UdpSocket::bind(endpoints.osc).unwrap();
    TcpListener::bind(endpoints.http).unwrap();
    service.start().unwrap();
    service.configure("192.0.2.1", 9000).unwrap();
    assert!(!service.query_enabled());
    assert!(service.endpoints().is_none());
    service.configure("localhost", 9000).unwrap();
    assert!(service.query_enabled());
    assert!(service.endpoints().is_some());
    service.shutdown();
    assert!(service.start().is_err());
}

struct Peer {
    daemon: ServiceDaemon,
    full_name: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Peer {
    fn new(port: u16, mute: bool, name: &str) -> Self {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let http_port = server.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let host_name = name.to_owned();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                if let Ok((mut stream, _)) = server.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0; 4096];
                    if let Ok(count) = stream.read(&mut request) {
                        let request = String::from_utf8_lossy(&request[..count]);
                        let body = if request.contains("HOST_INFO") {
                            json!({"NAME":host_name})
                        } else {
                            json!({"FULL_PATH":MUTE_ADDRESS,"VALUE":[mute]})
                        }
                        .to_string();
                        let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes());
                    }
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        });
        let daemon = ServiceDaemon::new_with_port(port).unwrap();
        let info = ServiceInfo::new(
            QUERY_SERVICE,
            name,
            &format!("{name}.local."),
            "127.0.0.1",
            http_port,
            None::<HashMap<String, String>>,
        )
        .unwrap();
        let full_name = info.get_fullname().to_string();
        daemon.register(info).unwrap();
        Self {
            daemon,
            full_name,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
        self.daemon
            .unregister(&self.full_name)
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        self.daemon
            .shutdown()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }
}

#[test]
fn real_mdns_advertisements_initial_mute_and_reconnect() {
    let port = test_port();
    let (sender, events) = mpsc::channel();
    let name = format!("VRChat-Client-test-{}", std::process::id());
    let service = OscQueryService::with_discovery_prefix(
        Arc::new(move |value| {
            let _ = sender.send(value);
        }),
        port,
        &name,
    );
    service.start().unwrap();
    let browser = ServiceDaemon::new_with_port(port).unwrap();
    let osc_events = browser.browse(OSC_SERVICE).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "native OSC advertisement missing"
        );
        if let Ok(ServiceEvent::ServiceResolved(info)) =
            osc_events.recv_timeout(Duration::from_secs(1))
        {
            if info.get_fullname().starts_with("VRCT-")
                && info.get_port() == service.endpoints().unwrap().osc.port()
            {
                break;
            }
        }
    }
    let peer = Peer::new(port, true, &name);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(15)).unwrap(),
        Some(true)
    );
    drop(peer);
    assert_eq!(events.recv_timeout(Duration::from_secs(15)).unwrap(), None);
    let peer = Peer::new(port, false, &name);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(15)).unwrap(),
        Some(false)
    );
    drop(peer);
    service.shutdown();
    browser
        .shutdown()
        .unwrap()
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
}
