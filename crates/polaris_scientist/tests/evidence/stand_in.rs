//! A small HTTP server on the loopback address that plays the reference
//! services. It answers one request at a time, records what it was asked, and
//! stops its thread when dropped.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// What the server sends back.
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn json(body: serde_json::Value) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    pub fn text(body: &str) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// A request target split into path and decoded query pairs.
pub struct Request {
    pub path: String,
    pub query: Vec<(String, String)>,
}

impl Request {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The running server.
pub struct StandIn {
    pub url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StandIn {
    pub fn start(route: impl Fn(&Request) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let Some(target) = read_target(&mut stream) else {
                        continue;
                    };
                    requests.lock().expect("log").push(target.clone());
                    let reply = route(&parse(&target));
                    let _ = write_reply(&mut stream, &reply);
                    let _ = stream.shutdown(Shutdown::Both);
                }
            })
        };
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// Request targets (`/path?query`) in the order they arrived.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("log").clone()
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let address = self.url.trim_start_matches("http://").to_string();
        let _ = TcpStream::connect(address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_target(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
        if head.len() > 16 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let first = text.lines().next()?;
    let mut parts = first.split_whitespace();
    let _method = parts.next()?;
    Some(parts.next()?.to_string())
}

fn parse(target: &str) -> Request {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Request {
        path: path.to_string(),
        query: query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(key), decode(value))
            })
            .collect(),
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut text = format!(
        "HTTP/1.1 {} Stand-in\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    text.push_str(&reply.body);
    stream.write_all(text.as_bytes())?;
    stream.flush()
}
