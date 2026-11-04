use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use dre_webhook::{never_sent, post, scrub};
use serde_json::{Map, json};

#[test]
fn only_errors_before_connection_are_safe_to_retry() {
    for error in [
        ureq::Error::HostNotFound,
        ureq::Error::ConnectionFailed,
        ureq::Error::Timeout(ureq::Timeout::Resolve),
        ureq::Error::Timeout(ureq::Timeout::Connect),
        ureq::Error::Io(std::io::ErrorKind::ConnectionRefused.into()),
    ] {
        assert!(never_sent(&error), "{error:?}");
    }
    for error in [
        ureq::Error::Timeout(ureq::Timeout::RecvResponse),
        ureq::Error::Io(std::io::ErrorKind::ConnectionReset.into()),
        ureq::Error::StatusCode(503),
    ] {
        assert!(!never_sent(&error), "{error:?}");
    }
}

#[test]
fn errors_remove_full_webhook_urls_and_their_bases() {
    let url = "https://example.test/private?key=secret&token=hidden";
    assert_eq!(
        scrub(
            &format!("request {url}; base https://example.test/private failed"),
            url
        ),
        "request <webhook_url>; base <webhook_url> failed"
    );
    assert_eq!(
        scrub(
            "failed https://example.test/private",
            "https://example.test/private"
        ),
        "failed <webhook_url>"
    );
}

#[test]
fn retryable_statuses_are_retried_without_changing_the_payload() {
    for status in [429, 503] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/hook?key=secret");
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            'requests: for status in [status, 202] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break 'requests;
                    }
                    headers.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let len: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                requests.push((headers, body));
                write!(stream, "HTTP/1.1 {status} Test\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            requests
        });
        let connection: Map<_, _> = json!({"retries": 1}).as_object().unwrap().clone();
        let payload = json!({"text": "hello"});
        let result = post(
            &url,
            &payload,
            &connection,
            "Test",
            "application/json; charset=UTF-8",
            |status| format!("rejected {status}"),
        );
        // Wake accept if a broken retry path returned after only the first request.
        drop(TcpStream::connect_timeout(&address, Duration::from_secs(5)));
        let requests = server.join().unwrap();
        result.unwrap();
        assert_eq!(requests.len(), 2);
        for (headers, body) in requests {
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("content-type: application/json; charset=utf-8\r\n")
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                payload
            );
        }
    }
}
