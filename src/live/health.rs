//! Whether `sensors live` is in its channels, for the container's health
//! check. The live site answers `/up` with 200 while it is and 503 while it
//! isn't, and `sensors health ADDRESS` asks it, since the container has no
//! curl.
//!
//! From lurker, the bot in wetfish/lurkers.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long the health check waits for an answer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The most of an answer the health check reads. Only the first line
/// matters.
const MAX_ANSWER: u64 = 4096;

/// Whether `sensors live` is connected to IRC and in all its channels,
/// shared with the live site.
#[derive(Clone, Default)]
pub struct Status(Arc<AtomicBool>);

impl Status {
    pub fn set_connected(&self, connected: bool) {
        self.0.store(connected, Ordering::Relaxed);
    }

    pub fn connected(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// `sensors health ADDRESS`: whether the live site at that address says it
/// is in its channels.
pub fn check(address: SocketAddr) -> Result<(), String> {
    let fail = |err: std::io::Error| format!("asking {address}: {err}");
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).map_err(fail)?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(fail)?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(fail)?;
    stream
        .write_all(b"GET /up HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(fail)?;

    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER)
        .read_to_end(&mut answer)
        .map_err(fail)?;
    let answer = String::from_utf8_lossy(&answer);
    let status = answer.lines().next().unwrap_or("");
    if status.split_whitespace().nth(1) == Some("200") {
        Ok(())
    } else {
        Err(format!("sensors live isn't in its channels ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    /// Answers one request with `answer`, and gives the request.
    fn answer_once(answer: &'static str) -> (SocketAddr, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let read = stream.read(&mut request).unwrap();
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        (address, handle)
    }

    #[test]
    fn asks_the_live_site_whether_it_is_up() {
        let (address, request) = answer_once("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nOK\n");
        assert_eq!(check(address), Ok(()));
        assert!(request.join().unwrap().starts_with("GET /up HTTP/1.1\r\n"));

        let (address, _) = answer_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
        let err = check(address).unwrap_err();
        assert!(err.contains("503 Service Unavailable"), "{err}");
    }

    #[test]
    fn explains_when_nothing_answers() {
        // Nothing listens on port 1.
        let err = check("127.0.0.1:1".parse().unwrap()).unwrap_err();
        assert!(err.starts_with("asking 127.0.0.1:1"), "{err}");
    }

    #[test]
    fn shares_whether_it_is_connected() {
        let status = Status::default();
        let shared = status.clone();
        assert!(!shared.connected());
        status.set_connected(true);
        assert!(shared.connected());
    }
}
