//! Listening-socket helpers shared by the sinks that run a local server
//! (WebSocket broadcast, OBS browser source).

use std::net::{IpAddr, TcpListener as StdTcpListener};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::watch;

/// A just-stopped server may still hold the port for a moment.
const BIND_ATTEMPTS: u32 = 25;
const BIND_RETRY: Duration = Duration::from_millis(40);

/// Never listen on every interface: the page and socket carry the user's
/// speech or the connection token, and the token is the only other protection.
pub fn is_wildcard(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_unspecified())
}

/// Bind, retrying briefly (a stop followed by a start on the same port),
/// and give up early if `shutdown` fires first.
pub async fn bind(host: &str, port: u16, shutdown: &mut watch::Receiver<bool>) -> Result<TcpListener, String> {
    let mut last = String::new();
    for _ in 0..BIND_ATTEMPTS {
        match StdTcpListener::bind((host, port)) {
            Ok(listener) => {
                listener.set_nonblocking(true).map_err(|e| e.to_string())?;
                return TcpListener::from_std(listener).map_err(|e| e.to_string());
            }
            Err(error) => last = error.to_string(),
        }
        tokio::select! {
            _ = shutdown.changed() => return Err("stopped before it could bind".into()),
            _ = tokio::time::sleep(BIND_RETRY) => {}
        }
    }
    Err(format!("cannot listen on {host}:{port}: {last}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_addresses_are_recognised() {
        assert!(is_wildcard("0.0.0.0"));
        assert!(is_wildcard("::"));
        assert!(!is_wildcard("127.0.0.1"));
        assert!(!is_wildcard("192.168.1.10"));
        assert!(!is_wildcard("localhost"));
    }
}
