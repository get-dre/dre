//! Registry and artifact downloads share the same transport policy.
use dre_core::context::{Context, Env};
use dre_core::manager::fetch;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn plaintext_remote_registry_and_artifact_urls_are_refused_before_connecting() {
    let ctx = Context::subprocess(Env::from_vars([] as [(&str, &str); 0]));
    for url in [
        "http://example.invalid/index.json",
        "http://example.invalid/plugin.tar.gz",
    ] {
        let error = fetch(&ctx, url).unwrap_err();
        assert!(error.contains("HTTPS"), "{error}");
    }
}

#[test]
fn oversized_downloads_are_refused_even_without_content_length() {
    const LIMIT: usize = 512 * 1024 * 1024;
    for declared_length in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).unwrap();
            if declared_length {
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", LIMIT + 1).unwrap();
            } else {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            let chunk = vec![0; 64 * 1024];
            for _ in 0..=LIMIT / chunk.len() {
                if socket.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let ctx = Context::subprocess(Env::from_vars([] as [(&str, &str); 0]));
        let result = fetch(&ctx, &url);
        server.join().unwrap();
        assert!(result.is_err(), "oversized download accepted");
        assert!(result.unwrap_err().contains("512 MiB"));
    }
}
