//! What the integration tests share: a local HTTP server that stands in for
//! YouTube, and temporary directories that clean up after themselves.
//!
//! The server is deliberately small. It answers each request on its own
//! connection, from a handler the test gives it, and keeps every request it
//! saw so the test can check what the code under test sent.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// One request, as the server read it.
#[derive(Clone, Debug)]
pub struct Request {
	pub method: String,
	/// The path and the query, e.g. `/a/b?c=d`.
	pub target: String,
	/// Header names in lower case.
	pub headers: Vec<(String, String)>,
	pub body: Vec<u8>,
}

impl Request {
	pub fn header(&self, name: &str) -> Option<&str> {
		self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
	}

	pub fn path(&self) -> &str {
		self.target.split('?').next().unwrap_or_default()
	}

	pub fn query(&self, key: &str) -> Option<&str> {
		let (_, query) = self.target.split_once('?')?;
		query.split('&').find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
	}

	pub fn json(&self) -> Value {
		serde_json::from_slice(&self.body).expect("request body is JSON")
	}
}

/// What the handler answers with.
pub struct Response {
	pub status: u16,
	pub content_type: Option<String>,
	pub body: Vec<u8>,
}

impl Response {
	pub fn ok(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
		Self {
			status: 200,
			content_type: Some(content_type.to_string()),
			body: body.into(),
		}
	}

	pub fn html(body: impl Into<String>) -> Self {
		Self::ok("text/html; charset=utf-8", body.into())
	}

	pub fn json(value: &Value) -> Self {
		Self::ok("application/json", value.to_string())
	}

	pub fn status(status: u16) -> Self {
		Self {
			status,
			content_type: Some("text/plain".to_string()),
			body: format!("status {status}").into_bytes(),
		}
	}
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// A server on a free local port. It runs until the test process ends.
pub struct MockServer {
	base: String,
	requests: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
	pub fn start(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
		let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
		let base = format!("http://{}", listener.local_addr().unwrap());
		let requests = Arc::new(Mutex::new(Vec::new()));
		let handler: Arc<Handler> = Arc::new(handler);

		let seen = Arc::clone(&requests);
		std::thread::spawn(move || {
			for stream in listener.incoming().flatten() {
				let handler = Arc::clone(&handler);
				let seen = Arc::clone(&seen);
				std::thread::spawn(move || serve(stream, &*handler, &seen));
			}
		});

		Self {
			base,
			requests,
		}
	}

	/// `path` on this server, e.g. `url("/watch")`.
	pub fn url(&self, path: &str) -> String {
		format!("{}{path}", self.base)
	}

	/// Every request so far, in the order they arrived.
	pub fn requests(&self) -> Vec<Request> {
		self.requests.lock().unwrap().clone()
	}

	/// The requests so far to `path`, query ignored.
	pub fn requests_to(&self, path: &str) -> Vec<Request> {
		self.requests().into_iter().filter(|r| r.path() == path).collect()
	}
}

fn serve(stream: TcpStream, handler: &Handler, seen: &Mutex<Vec<Request>>) {
	let Some(request) = read_request(&stream) else {
		return;
	};
	let response = handler(&request);
	seen.lock().unwrap().push(request);

	let mut out = &stream;
	let mut head = format!("HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n", response.status, response.body.len());
	if let Some(content_type) = &response.content_type {
		head.push_str(&format!("content-type: {content_type}\r\n"));
	}
	head.push_str("\r\n");
	let _ = out.write_all(head.as_bytes());
	let _ = out.write_all(&response.body);
	let _ = out.flush();
}

fn read_request(stream: &TcpStream) -> Option<Request> {
	let mut reader = BufReader::new(stream);
	let mut line = String::new();
	reader.read_line(&mut line).ok()?;
	let mut parts = line.split_whitespace();
	let method = parts.next()?.to_string();
	let target = parts.next()?.to_string();

	let mut headers = Vec::new();
	loop {
		let mut line = String::new();
		reader.read_line(&mut line).ok()?;
		let line = line.trim_end();
		if line.is_empty() {
			break;
		}
		let (name, value) = line.split_once(':')?;
		headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
	}

	let length = headers.iter().find(|(n, _)| n == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
	let mut body = vec![0; length];
	reader.read_exact(&mut body).ok()?;
	Some(Request {
		method,
		target,
		headers,
		body,
	})
}

/// A directory under the system temp directory, removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
	pub fn new(name: &str) -> Self {
		static NEXT: AtomicUsize = AtomicUsize::new(0);
		let n = NEXT.fetch_add(1, Ordering::Relaxed);
		let dir = std::env::temp_dir().join(format!("yta-it-{name}-{}-{n}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		Self(dir)
	}

	pub fn path(&self) -> &Path {
		&self.0
	}
}

impl Drop for TempDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// The lines of a JSONL file, parsed.
pub fn read_jsonl(path: &Path) -> Vec<Value> {
	std::fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).expect("a JSON line")).collect()
}
