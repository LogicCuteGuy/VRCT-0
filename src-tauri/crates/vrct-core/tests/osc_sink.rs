//! The OSC sink reproduces python-osc bytes at the configured destination.
//! `fixtures/osc_golden.json` captures the historical Python `OSCHandler`
//! contract, frozen at `16cb286c`; native test commands: `fixtures/README.md`.

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::sinks::osc::{message_packet, typing_packet};
use vrct_core::sinks::Sinks;

const GOLDEN: &str = include_str!("fixtures/osc_golden.json");

fn golden() -> Vec<Value> {
    serde_json::from_str(GOLDEN).expect("golden fixture is JSON")
}

fn line(endpoint: &str, result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": endpoint, "result": result}).to_string();
    parse_sidecar_line(&text).expect("sidecar line parses")
}

fn replica_pointing_at(port: u16) -> Arc<ConfigReplica> {
    let replica = Arc::new(ConfigReplica::default());
    replica.ingest(&line(
        "/internal/config/snapshot",
        json!({"OSC_IP_ADDRESS": "127.0.0.1", "OSC_PORT": port}),
    ));
    replica
}

fn receiver() -> (UdpSocket, u16) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let port = socket.local_addr().unwrap().port();
    (socket, port)
}

fn receive(socket: &UdpSocket) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let len = socket.recv(&mut buf).expect("a packet arrives");
    buf[..len].to_vec()
}

#[test]
fn encoding_is_byte_identical_to_python_osc() {
    let cases = golden();
    assert!(cases.len() > 10, "fixture looks empty");
    for case in cases {
        let expected = case["packet"].as_str().map(|hex| hex::decode(hex).unwrap());
        match case["kind"].as_str().unwrap() {
            "typing" => {
                let packet = typing_packet(case["flag"].as_bool().unwrap());
                assert_eq!(Some(packet), expected, "{case}");
            }
            "message" => {
                let message = case["message"].as_str().unwrap();
                // Python sends nothing for an empty message; the sink drops it
                // before encoding, so there is no packet to compare.
                if message.is_empty() {
                    assert_eq!(expected, None, "{case}");
                    continue;
                }
                let packet = message_packet(message, case["notification"].as_bool().unwrap());
                assert_eq!(Some(packet), expected, "{case}");
            }
            other => panic!("unknown case kind {other}"),
        }
    }
}

#[test]
fn a_sink_line_reaches_the_configured_port() {
    let (socket, port) = receiver();
    let sinks = Sinks::new(replica_pointing_at(port));

    assert!(sinks.ingest(&line("/internal/osc/typing", json!({"flag": true}))));
    assert_eq!(receive(&socket), typing_packet(true));

    let consumed = sinks.ingest(&line(
        "/internal/osc/message",
        json!({"message": "こんにちは", "notification": false}),
    ));
    assert!(consumed);
    assert_eq!(receive(&socket), message_packet("こんにちは", false));
}

#[test]
fn a_settings_change_applies_to_the_next_packet() {
    let (first, first_port) = receiver();
    let (second, second_port) = receiver();
    let replica = replica_pointing_at(first_port);
    let sinks = Sinks::new(Arc::clone(&replica));

    sinks.ingest(&line("/internal/osc/typing", json!({"flag": true})));
    assert_eq!(receive(&first), typing_packet(true));

    replica.ingest(&line(
        "/internal/config/changed",
        json!({"key": "OSC_PORT", "value": second_port}),
    ));
    sinks.ingest(&line("/internal/osc/typing", json!({"flag": false})));
    assert_eq!(receive(&second), typing_packet(false));
}

#[test]
fn an_empty_message_sends_nothing() {
    let (socket, port) = receiver();
    socket.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let sinks = Sinks::new(replica_pointing_at(port));

    assert!(sinks.ingest(&line(
        "/internal/osc/message",
        json!({"message": "", "notification": true}),
    )));
    let mut buf = [0u8; 64];
    assert!(socket.recv(&mut buf).is_err(), "nothing should have been sent");
}

#[test]
fn sink_lines_are_consumed_even_when_unusable() {
    // Unknown target (replica still empty) and a malformed body: neither may
    // reach the UI, which would treat the endpoint as invalid.
    let sinks = Sinks::new(Arc::new(ConfigReplica::default()));
    assert!(sinks.ingest(&line("/internal/osc/typing", json!({"flag": true}))));
    assert!(sinks.ingest(&line("/internal/osc/message", json!({"oops": 1}))));
}

#[test]
fn other_lines_are_left_alone() {
    let sinks = Sinks::new(Arc::new(ConfigReplica::default()));
    assert!(!sinks.ingest(&line("/internal/config/changed", json!({"key": "A", "value": 1}))));
    assert!(!sinks.ingest(&line("/run/initialization_complete", json!(true))));
}
