use super::*;

pub(super) fn start_provider(
    dir: &Path,
    stopping: bool,
) -> (
    std::sync::mpsc::Receiver<String>,
    thread::JoinHandle<()>,
    PathBuf,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let server = if stopping {
        thread::spawn(move || serve_stop_fixture(listener, request_tx))
    } else {
        start_initial_server(listener, request_tx)
    };
    let profile = dir.join("loopback-profile.json");
    fs::write(&profile, serde_json::json!({"provider":"openai", "model":"loopback", "ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"test"}}).to_string()).unwrap();
    (request_rx, server, profile)
}

fn start_initial_server(
    listener: TcpListener,
    request_tx: std::sync::mpsc::Sender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let bytes = read_initial_request(&mut stream);
                    request_tx
                        .send(String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap();
                    let body = r#"{"id":"chatcmpl-test","object":"chat.completion","created":0,"model":"loopback","choices":[{"index":0,"message":{"role":"assistant","content":"Hello."},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    stream.flush().unwrap();
                    return;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("loopback provider did not receive a request: {error}"),
            }
        }
    })
}

fn serve_stop_fixture(
    listener: std::net::TcpListener,
    request_tx: std::sync::mpsc::Sender<String>,
) {
    use std::{
        io::Write,
        thread,
        time::{Duration, Instant},
    };

    let deadline = Instant::now() + Duration::from_secs(20);
    for _ in 0..2 {
        loop {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(20))
                    }
                    Err(error) => panic!(
                        "loopback provider did not receive a request before deadline: {error}"
                    ),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let bytes = read_stop_request(&mut stream);
            if bytes.is_empty() {
                continue;
            }
            request_tx.send(stop_request_summary(&bytes)).unwrap();
            let body = r#"{"id":"chatcmpl-test","object":"chat.completion","created":0,"model":"loopback","choices":[{"index":0,"message":{"role":"assistant","content":"Hello again."},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            stream.flush().unwrap();
            break;
        }
    }
}

fn read_initial_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if request_complete(&bytes) {
            break;
        }
    }
    bytes
}
fn read_stop_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) if bytes.is_empty() => break,
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                panic!("loopback provider request read timed out")
            }
            Err(error) => panic!("loopback provider request read failed: {error}"),
        }
        if request_complete(&bytes) {
            break;
        }
    }
    bytes
}
fn stop_request_summary(bytes: &[u8]) -> String {
    let split = bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap_or_else(|| panic!("loopback provider received nonempty malformed request without header terminator ({} bytes)", bytes.len()));
    let headers = String::from_utf8_lossy(&bytes[..split]);
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("POST "))
    {
        panic!(
            "loopback provider received malformed request line: {}",
            headers.lines().next().unwrap_or("<empty>")
        );
    }
    let request_line = headers.lines().next().unwrap();
    let body_start = split + 4;
    let request_body = String::from_utf8_lossy(&bytes[body_start..])
        .chars()
        .take(8192)
        .collect::<String>();
    format!("{request_line}\n{request_body}")
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(split) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..split]);
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    bytes.len() >= split + 4 + length
}
