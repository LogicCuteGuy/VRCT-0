//! VRChat chatbox output over OSC (UDP).
//!
//! Wire format is OSC 1.0 with boolean type tags, byte-for-byte what
//! python-osc's `SimpleUDPClient` sent, so VRChat cannot tell the difference
//! (`tests/fixtures/osc_golden.json` pins that).

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex};

use crate::config::ConfigReplica;

const TYPING_ADDRESS: &str = "/chatbox/typing";
const INPUT_ADDRESS: &str = "/chatbox/input";

enum Arg<'a> {
    Str(&'a str),
    Bool(bool),
}

fn pad_to_four(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

fn push_string(buf: &mut Vec<u8>, text: &str) {
    // An embedded NUL would end the string early on the receiving side.
    buf.extend(text.bytes().filter(|byte| *byte != 0));
    buf.push(0);
    pad_to_four(buf);
}

fn encode(address: &str, args: &[Arg]) -> Vec<u8> {
    let mut buf = Vec::new();
    push_string(&mut buf, address);

    let mut tags = String::from(",");
    for arg in args {
        tags.push(match arg {
            Arg::Str(_) => 's',
            Arg::Bool(true) => 'T',
            Arg::Bool(false) => 'F',
        });
    }
    push_string(&mut buf, &tags);

    for arg in args {
        if let Arg::Str(text) = arg {
            push_string(&mut buf, text);
        }
    }
    buf
}

pub fn typing_packet(flag: bool) -> Vec<u8> {
    encode(TYPING_ADDRESS, &[Arg::Bool(flag)])
}

pub fn message_packet(message: &str, notification: bool) -> Vec<u8> {
    encode(
        INPUT_ADDRESS,
        &[Arg::Str(message), Arg::Bool(true), Arg::Bool(notification)],
    )
}

/// Resolve `host:port`, IPv4 first. `localhost` can resolve to `::1` before
/// `127.0.0.1` on Windows, while VRChat listens on the IPv4 loopback.
fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let mut addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve OSC address {host}:{port}: {error}"))?
        .collect();
    addresses.sort_by_key(|address| address.is_ipv6());
    addresses
        .into_iter()
        .next()
        .ok_or_else(|| format!("OSC address {host}:{port} has no usable address"))
}

/// Sends chatbox packets to whatever `OSC_IP_ADDRESS`/`OSC_PORT` currently
/// are in the config replica, so a settings change applies to the next packet.
pub struct OscSink {
    replica: Arc<ConfigReplica>,
    v4: Mutex<Option<UdpSocket>>,
    v6: Mutex<Option<UdpSocket>>,
}

impl OscSink {
    pub fn new(replica: Arc<ConfigReplica>) -> Self {
        Self {
            replica,
            v4: Mutex::new(None),
            v6: Mutex::new(None),
        }
    }

    pub fn typing(&self, flag: bool) -> Result<(), String> {
        self.send(&typing_packet(flag))
    }

    /// Empty messages are dropped, as python-osc's wrapper did.
    pub fn message(&self, message: &str, notification: bool) -> Result<(), String> {
        if message.is_empty() {
            return Ok(());
        }
        self.send(&message_packet(message, notification))
    }

    fn target(&self) -> Result<SocketAddr, String> {
        let host = self
            .replica
            .get_str("OSC_IP_ADDRESS")
            .ok_or("OSC target is not known yet")?;
        let port = self
            .replica
            .get("OSC_PORT")
            .and_then(|value| value.as_u64())
            .and_then(|port| u16::try_from(port).ok())
            .ok_or("OSC port is not known yet")?;
        resolve(&host, port)
    }

    fn send(&self, packet: &[u8]) -> Result<(), String> {
        let target = self.target()?;
        let (slot, bind) = if target.is_ipv4() {
            (&self.v4, "0.0.0.0:0")
        } else {
            (&self.v6, "[::]:0")
        };
        let mut socket = slot.lock().unwrap();
        if socket.is_none() {
            *socket = Some(UdpSocket::bind(bind).map_err(|error| error.to_string())?);
        }
        socket
            .as_ref()
            .expect("bound above")
            .send_to(packet, target)
            .map(|_| ())
            .map_err(|error| format!("OSC send to {target} failed: {error}"))
    }
}
