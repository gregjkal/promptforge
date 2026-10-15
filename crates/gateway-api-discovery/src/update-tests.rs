//! The update request against a fixture listener: the request it sends,
//! the status and body it returns, the answer cap, and its failures.

use super::*;

use std::io::{Read, Write as _};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

/// A fixture gateway that records one request, answers with `response`,
/// and closes the connection, as the HTTP/1.0 server does.
fn fixture_gateway(response: Vec<u8>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let address = listener.local_addr().expect("fixture address").to_string();
    let (requests, received) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 1024];
            if let Ok(read) = stream.read(&mut buffer) {
                let _ = requests.send(String::from_utf8_lossy(&buffer[..read]).into_owned());
            }
            let _ = stream.write_all(&response);
        }
    });
    (address, received)
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn the_update_posts_the_route_with_the_key_and_returns_status_and_body() {
    let body = br#"{"outcome":"updating","version":"0.4.0"}"#;
    let mut response = format!(
        "HTTP/1.1 202 Accepted\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    let (address, received) = fixture_gateway(response);
    let answer = request_update_at(&address, "key", soon()).expect("the gateway answered");
    assert_eq!(
        answer,
        UpdateResponse {
            status: 202,
            body: body.to_vec(),
        }
    );
    let request = received
        .recv_timeout(Duration::from_secs(5))
        .expect("the request arrived");
    assert!(
        request.starts_with("POST /update HTTP/1.0\r\n"),
        "the request is a POST to the update route: {request:?}"
    );
    assert!(
        request.contains("Authorization: Bearer key\r\n"),
        "the file's key is the bearer token: {request:?}"
    );
    assert!(
        request.contains(&format!("Host: {address}\r\n")),
        "the Host header is the bound address: {request:?}"
    );
}

#[test]
fn an_error_status_is_returned_with_its_body() {
    let (address, _received) =
        fixture_gateway(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec());
    let answer = request_update_at(&address, "key", soon()).expect("a 404 is an answer");
    assert_eq!(answer.status, 404);
    assert!(answer.body.is_empty());
}

#[test]
fn an_answer_over_the_cap_is_cut_at_16_kib() {
    let mut response = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    let head = response.len();
    response.extend(std::iter::repeat_n(b'x', 64 * 1024));
    let (address, _received) = fixture_gateway(response);
    let answer = request_update_at(&address, "key", soon()).expect("the gateway answered");
    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.body.len(),
        ANSWER_LIMIT - head,
        "the read stops at the cap"
    );
}

#[test]
fn a_refused_connection_is_an_io_error() {
    // Port 1 is never listening, so the connect fails fast.
    let error = request_update_at("127.0.0.1:1", "key", soon()).expect_err("nothing listens");
    assert!(
        matches!(error, UpdateRequestError::Io { .. }),
        "an undelivered request is an I/O error: {error}"
    );
}

#[test]
fn an_answer_without_a_status_line_is_malformed() {
    let (address, _received) = fixture_gateway(b"not http".to_vec());
    let error = request_update_at(&address, "key", soon()).expect_err("not HTTP");
    assert!(matches!(error, UpdateRequestError::Malformed), "{error}");
}

#[test]
fn the_deadline_bounds_a_gateway_that_never_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow fixture");
    let address = listener.local_addr().expect("fixture address").to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the update");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        std::thread::sleep(Duration::from_secs(2));
    });
    let started = Instant::now();
    let error = request_update_at(&address, "key", started + Duration::from_millis(100))
        .expect_err("no answer within the deadline");
    assert!(matches!(error, UpdateRequestError::Io { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the request ends at its deadline"
    );
}

#[test]
fn a_reflected_bearer_is_absent_from_update_errors() {
    let secret = "capability-secret";
    let (address, _received) = fixture_gateway(format!("rejected-{secret}").into_bytes());
    let error = request_update_at(&address, secret, soon()).expect_err("not HTTP");
    assert!(
        !format!("{error:?} {error}").contains(secret),
        "bearer values never enter error diagnostics"
    );
}
