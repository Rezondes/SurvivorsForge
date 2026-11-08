//! A tiny HTTP server for tests: answers each request in turn with the next canned response and
//! hands every request (request line, headers, body) back to the test.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, channel};

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub line: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// One canned answer: status, extra headers, body.
pub type Answer = (u16, Vec<(&'static str, String)>, String);

/// Serves `answers` one per connection; returns the base URL and the
/// requests. Requests beyond the answers get no answer.
pub fn serve(answers: Vec<Answer>) -> (String, Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for (status, headers, body) in answers {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut seen = Seen { line: line.trim_end().to_string(), headers: Vec::new(), body: String::new() };
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((n, v)) = h.split_once(':') {
                    seen.headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
                }
            }
            let length = seen.header("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            seen.body = String::from_utf8_lossy(&bytes).into_owned();
            let _ = tx.send(seen);
            let extra: String = headers.iter().map(|(n, v)| format!("{n}: {v}\r\n")).collect();
            let answer = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len());
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
    (url, rx)
}
