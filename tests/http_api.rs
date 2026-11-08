//! The plain HTTP calls every browser-free path is built on: what they send,
//! what they hand back, and how they fail.

mod common;

use common::{MockServer, Response};
use serde_json::json;
use yt_meta_archiver::http;

#[test]
fn get_html_poses_as_an_english_desktop_browser() {
	let server = MockServer::start(|_| Response::html("<html>hello</html>"));

	let html = http::get_html(&server.url("/watch?v=dQw4w9WgXcQ")).unwrap();
	assert_eq!(html, "<html>hello</html>");

	let [request] = server.requests().try_into().unwrap();
	assert_eq!(request.method, "GET");
	assert_eq!(request.target, "/watch?v=dQw4w9WgXcQ");
	assert!(request.header("user-agent").is_some_and(|ua| ua.contains("Chrome/")), "{:?}", request.header("user-agent"));
	assert_eq!(request.header("accept-language"), Some("en-US,en"));
}

#[test]
fn get_html_reads_a_page_of_youtube_size() {
	// A real watch page is about 750 KB, with the data we want near the end.
	let page = format!("{}<end>", "x".repeat(1024 * 1024));
	let body = page.clone();
	let server = MockServer::start(move |_| Response::html(body.clone()));
	assert_eq!(http::get_html(&server.url("/big")).unwrap(), page);
}

#[test]
fn get_bytes_returns_the_body_and_its_content_type() {
	let image = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 255];
	let body = image.clone();
	let server = MockServer::start(move |_| Response::ok("image/png", body.clone()));

	let (bytes, content_type) = http::get_bytes(&server.url("/a.png")).unwrap();
	assert_eq!(bytes, image);
	assert_eq!(content_type.as_deref(), Some("image/png"));
}

#[test]
fn get_bytes_without_a_content_type_says_so() {
	let server = MockServer::start(|_| Response {
		status: 200,
		content_type: None,
		body: b"raw".to_vec(),
	});
	let (bytes, content_type) = http::get_bytes(&server.url("/raw")).unwrap();
	assert_eq!(bytes, b"raw");
	assert_eq!(content_type, None);
}

#[test]
fn post_json_sends_json_from_youtube_and_parses_the_answer() {
	let server = MockServer::start(|request| {
		let sent = request.json();
		Response::json(&json!({"echo": sent["continuation"], "ok": true}))
	});

	let body = json!({"context": {"client": {"clientName": "WEB"}}, "continuation": "abc"});
	let answer = http::post_json(&server.url("/youtubei/v1/x?key=k"), &body).unwrap();
	assert_eq!(answer, json!({"echo": "abc", "ok": true}));

	let [request] = server.requests().try_into().unwrap();
	assert_eq!(request.method, "POST");
	assert_eq!(request.query("key"), Some("k"));
	assert_eq!(request.header("content-type"), Some("application/json"));
	// The endpoint answers a bare 400 without this.
	assert_eq!(request.header("origin"), Some("https://www.youtube.com"));
	assert_eq!(request.json(), body);
}

#[test]
fn an_error_status_is_an_error() {
	let server = MockServer::start(|request| match request.path() {
		"/missing" => Response::status(404),
		_ => Response::status(500),
	});
	assert!(http::get_html(&server.url("/missing")).is_err());
	assert!(http::get_bytes(&server.url("/missing")).is_err());
	assert!(http::post_json(&server.url("/broken"), &json!({})).is_err());
}

#[test]
fn an_answer_that_is_not_json_is_an_error() {
	let server = MockServer::start(|_| Response::html("<html>not json</html>"));
	assert!(http::post_json(&server.url("/api"), &json!({})).is_err());
}

#[test]
fn a_server_that_is_not_there_is_an_error() {
	// Bind a port, then free it, so nothing is listening on it.
	let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
	// One call only: Windows takes about two seconds to refuse each one.
	assert!(http::get_html(&format!("http://127.0.0.1:{port}/")).is_err());
}
