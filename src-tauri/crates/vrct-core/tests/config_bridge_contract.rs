//! The replica understands what the historical Python `Config` wrote.
//! `fixtures/config_bridge.jsonl` is the frozen contract captured at `16cb286c`.
//! Provenance and native test commands: `fixtures/README.md`.

use serde_json::json;
use vrct_core::config::{ConfigReplica, SIMPLE_GETTERS};
use vrct_core::protocol::parse_sidecar_line;

const FIXTURE: &str = include_str!("fixtures/config_bridge.jsonl");

fn replica_after_fixture() -> ConfigReplica {
    let replica = ConfigReplica::default();
    for line in FIXTURE.lines() {
        let response = parse_sidecar_line(line).expect("sidecar line parses");
        assert!(replica.ingest(&response), "bridge line must be consumed: {line}");
    }
    replica
}

#[test]
fn every_served_getter_is_answerable_from_a_real_snapshot() {
    let replica = replica_after_fixture();
    let missing: Vec<_> = SIMPLE_GETTERS
        .iter()
        .filter(|(_, key)| !replica.contains(key))
        .collect();
    assert!(missing.is_empty(), "keys Python never sends: {missing:?}");
}

#[test]
fn changes_after_the_snapshot_are_applied() {
    let replica = replica_after_fixture();
    assert_eq!(replica.get_str("UI_LANGUAGE").as_deref(), Some("ja"));
    assert_eq!(replica.get("MIC_WORD_FILTER"), Some(json!(["a", "b"])));
}

#[test]
fn numeric_types_survive_the_trip() {
    let replica = replica_after_fixture();
    assert_eq!(replica.get("OSC_PORT"), Some(json!(9000)));
    assert_eq!(replica.get("MIC_AVG_LOGPROB"), Some(json!(-0.8)));
    assert_eq!(replica.get("OVERLAY_SMALL_LOG_SETTINGS").unwrap()["opacity"], json!(1.0));
}
