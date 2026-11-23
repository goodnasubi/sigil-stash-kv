use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sigil(root: &Path, args: &[&str]) -> Output {
    let executable = env::var_os("SIGIL").unwrap_or_else(|| "sigil".into());
    let output = Command::new(&executable)
        .current_dir(root)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run {executable:?}: {error}"));
    assert!(
        output.status.success(),
        "sigil {args:?} failed with {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

struct ServerProcess {
    child: Child,
    port: u16,
    binary: PathBuf,
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.binary);
    }
}

fn start_server(root: &Path) -> ServerProcess {
    let out = root.join("target");
    fs::create_dir_all(&out).expect("create target dir");
    let binary = out.join(format!("tcp-server-{}", std::process::id()));

    sigil(
        root,
        &[
            "build",
            "--root",
            ".",
            "tests/tcp_server_main.sigil",
            "main",
            "-o",
            binary.to_str().expect("binary path utf-8"),
        ],
    );

    let mut child = Command::new(&binary)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn server process");

    let stdout = child.stdout.take().expect("capture server stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read server port");
    let port: u16 = line
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("invalid port output {line:?}: {e}"));

    ServerProcess {
        child,
        port,
        binary,
    }
}

fn encode_req(op: u8, body: &[u8]) -> Vec<u8> {
    let payload_len = 2 + body.len();
    assert!(payload_len <= 4096);
    let mut frame = Vec::new();
    frame.extend_from_slice(&(payload_len as u32).to_be_bytes());
    frame.push(1); // Version 1
    frame.push(op);
    frame.extend_from_slice(body);
    frame
}

fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    stream.read_exact(buf)
}

fn read_response(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut len_buf = [0u8; 4];
    read_exact(stream, &mut len_buf)?;
    let length = u32::from_be_bytes(len_buf) as usize;
    if !(2..=4096).contains(&length) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid length {length}"),
        ));
    }
    let mut payload = vec![0u8; length];
    read_exact(stream, &mut payload)?;
    let version = payload[0];
    if version != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported version {version}"),
        ));
    }
    let res_type = payload[1];
    let body = payload[2..].to_vec();
    Ok((res_type, body))
}

#[test]
fn test_protocol_sigil_checks() {
    let root = manifest_dir();
    sigil(
        &root,
        &[
            "fmt",
            "--check",
            "stash/protocol.sigil",
            "stash/client.sigil",
            "stash/server.sigil",
            "tests/protocol.sigil",
            "tests/tcp_server_main.sigil",
            "tests/tcp_client_test.sigil",
        ],
    );
    sigil(&root, &["check", "--root", ".", "stash/protocol.sigil"]);
    sigil(&root, &["check", "--root", ".", "stash/client.sigil"]);
    sigil(&root, &["check", "--root", ".", "stash/server.sigil"]);
    sigil(&root, &["check", "--root", ".", "tests/protocol.sigil"]);
    sigil(
        &root,
        &["check", "--root", ".", "tests/tcp_server_main.sigil"],
    );
    sigil(
        &root,
        &["check", "--root", ".", "tests/tcp_client_test.sigil"],
    );

    // Run tests/protocol.sigil in evaluator
    let out = sigil(
        &root,
        &["run", "--root", ".", "tests/protocol.sigil", "main"],
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");

    // Run tests/protocol.sigil in native
    let out_dir = root.join("target");
    let proto_bin = out_dir.join(format!("test-proto-{}", std::process::id()));
    sigil(
        &root,
        &[
            "build",
            "--root",
            ".",
            "tests/protocol.sigil",
            "main",
            "-o",
            proto_bin.to_str().unwrap(),
        ],
    );
    let status = Command::new(&proto_bin).status().unwrap();
    let _ = fs::remove_file(proto_bin);
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_loopback_integration() {
    let root = manifest_dir();
    let server = start_server(&root);
    let addr = format!("127.0.0.1:{}", server.port);

    // 1. PING
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let req = encode_req(1, &[]);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1, "PING expected SUCCESS (1)");
        assert_eq!(body.len(), 0);
    }

    // 2. PUT without TTL, GET, DELETE, GET (Not Found)
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        // PUT key="alpha", val="beta", ttl_tag=0
        let key = b"alpha";
        let val = b"beta";
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&(key.len() as u16).to_be_bytes());
        put_body.extend_from_slice(key);
        put_body.extend_from_slice(&(val.len() as u16).to_be_bytes());
        put_body.extend_from_slice(val);
        put_body.push(0); // NoExpiry
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1, "PUT expected SUCCESS (1)");

        // GET key="alpha"
        let mut get_body = Vec::new();
        get_body.extend_from_slice(&(key.len() as u16).to_be_bytes());
        get_body.extend_from_slice(key);
        let req = encode_req(2, &get_body);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 2, "GET expected VALUE (2)");
        assert_eq!(u16::from_be_bytes([body[0], body[1]]), val.len() as u16);
        assert_eq!(&body[2..], val);

        // DELETE key="alpha"
        let req = encode_req(4, &get_body);
        stream.write_all(&req).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1, "DELETE expected SUCCESS (1)");

        // GET key="alpha" -> NOT_FOUND (3)
        let req = encode_req(2, &get_body);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 3, "GET expected NOT_FOUND (3)");
        assert_eq!(body.len(), 0);
    }

    // 3. PUT with TTL (1 second) and check expiry
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        let key = b"temp_key";
        let val = b"temp_val";
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&(key.len() as u16).to_be_bytes());
        put_body.extend_from_slice(key);
        put_body.extend_from_slice(&(val.len() as u16).to_be_bytes());
        put_body.extend_from_slice(val);
        put_body.push(1); // Seconds
        put_body.extend_from_slice(&1i64.to_be_bytes()); // 1 second TTL
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1);

        // Immediately GET -> should exist
        let mut get_body = Vec::new();
        get_body.extend_from_slice(&(key.len() as u16).to_be_bytes());
        get_body.extend_from_slice(key);
        stream.write_all(&encode_req(2, &get_body)).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 2);
        assert_eq!(&body[2..], val);

        // Wait for expiry (2.1 seconds to guarantee cross-second boundary in unix_seconds)
        thread::sleep(Duration::from_millis(2100));

        // GET -> should be NOT_FOUND
        stream.write_all(&encode_req(2, &get_body)).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 3, "expired key expected NOT_FOUND (3)");
    }

    // 4. Fragmented writes (1 byte at a time)
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let key = b"frag_key";
        let mut get_body = Vec::new();
        get_body.extend_from_slice(&(key.len() as u16).to_be_bytes());
        get_body.extend_from_slice(key);
        let req = encode_req(2, &get_body);

        for byte in req {
            stream.write_all(&[byte]).unwrap();
            thread::sleep(Duration::from_millis(2));
        }

        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 3, "GET of absent key expected NOT_FOUND");
    }

    // 5. Invalid frame length (< 2 and > 4096)
    {
        // Length = 1 (< 2)
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let bad_len_frame = [0, 0, 0, 1, 1];
        stream.write_all(&bad_len_frame).unwrap();
        let mut buf = [0u8; 1];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0, "server must disconnect on length < 2");

        // Length = 4097 (> 4096)
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let bad_len_frame = [0, 0, 0x10, 0x01, 1, 1];
        stream.write_all(&bad_len_frame).unwrap();
        let n = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0, "server must disconnect on length > 4096");
    }

    // 6. Unsupported version (Version 2)
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let bad_ver_frame = [0, 0, 0, 2, 2, 1]; // Length 2, Version 2, Op 1
        stream.write_all(&bad_ver_frame).unwrap();
        let mut buf = [0u8; 1];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0, "server must disconnect on unsupported version");
    }

    // 7. Unknown Op (Op 99) -> Server returns ERROR (Code 1), connection stays alive
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let bad_op_frame = [0, 0, 0, 2, 1, 99]; // Length 2, Version 1, Op 99
        stream.write_all(&bad_op_frame).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 4, "unknown op expected ERROR (4)");
        assert_eq!(body[0], 1, "error code 1 (UnknownOp)");

        // Connection should remain alive: send PING
        let req = encode_req(1, &[]);
        stream.write_all(&req).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1, "subsequent PING expected SUCCESS");
    }

    // 8. Invalid Payload (malformed TTL tag) -> ERROR (Code 2), connection stays alive
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&1u16.to_be_bytes());
        put_body.push(b'k');
        put_body.extend_from_slice(&1u16.to_be_bytes());
        put_body.push(b'v');
        put_body.push(99); // bad ttl tag
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 4, "invalid payload expected ERROR (4)");
        assert_eq!(body[0], 2, "error code 2 (InvalidPayload)");

        // Connection still alive
        stream.write_all(&encode_req(1, &[])).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1);
    }

    // 9. Key and Value limits
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        // Key length = 0 (empty)
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&0u16.to_be_bytes()); // key_len = 0
        put_body.extend_from_slice(&1u16.to_be_bytes());
        put_body.push(b'v');
        put_body.push(0);
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 4);
        assert_eq!(body[0], 3, "error code 3 (KeyLength)");

        // Key length = 513 (> 512)
        let big_key = vec![b'k'; 513];
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&(big_key.len() as u16).to_be_bytes());
        put_body.extend_from_slice(&big_key);
        put_body.extend_from_slice(&1u16.to_be_bytes());
        put_body.push(b'v');
        put_body.push(0);
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 4);
        assert_eq!(body[0], 3, "error code 3 (KeyLength)");
    }

    // 10. Truncated disconnects
    {
        // Client disconnects after sending only 2 bytes of header
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream.write_all(&[0, 0]).unwrap();
        let _ = stream.shutdown(Shutdown::Both);

        // Client disconnects after sending header but incomplete payload
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream.write_all(&[0, 0, 0, 10, 1, 2, 0]).unwrap();
        let _ = stream.shutdown(Shutdown::Both);

        // Server must remain healthy: send PING on a new connection
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(&encode_req(1, &[])).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(
            res_type, 1,
            "server remains healthy after client disconnects"
        );
    }

    // 11. Fixtures verification
    {
        let fix_dir = root.join("tests/fixtures");
        if fix_dir.exists() {
            // Test valid ping fixture
            let ping_data = fs::read(fix_dir.join("req_ping.bin")).expect("read req_ping.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&ping_data).unwrap();
            let (res_type, _) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 1);

            // Test max frame boundary fixture (length 4096)
            let max_data =
                fs::read(fix_dir.join("req_max_frame.bin")).expect("read req_max_frame.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&max_data).unwrap();
            // Should get error (ValueLength > 3072) or success, but connection handled cleanly
            let (res_type, body) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 4);
            assert_eq!(body[0], 4, "ValueLength error for 4088 byte value");
        }
    }

    // 12. Sigil client integration test (native)
    {
        let out_dir = root.join("target");
        let client_bin = out_dir.join(format!("test-client-{}", std::process::id()));
        sigil(
            &root,
            &[
                "build",
                "--root",
                ".",
                "tests/tcp_client_test.sigil",
                "main",
                "-o",
                client_bin.to_str().unwrap(),
            ],
        );
        let status = Command::new(&client_bin)
            .arg(server.port.to_string())
            .status()
            .unwrap();
        let _ = fs::remove_file(client_bin);
        assert_eq!(
            status.code(),
            Some(0),
            "Sigil client native test failed with code {:?}",
            status.code()
        );
    }

    // 13. Read timeout (5000 ms) test: server closes inactive connection
    {
        let mut stream = TcpStream::connect(&addr).expect("connect for timeout test");
        // Don't send anything; wait for server 5000ms timeout
        stream
            .set_read_timeout(Some(Duration::from_millis(6000)))
            .unwrap();
        let mut buf = [0u8; 1];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(
            n, 0,
            "server must disconnect client after 5000ms read timeout"
        );
    }

    // 14. Concurrent connections test
    {
        let mut conns = Vec::new();
        for _ in 0..16 {
            if let Ok(mut s) = TcpStream::connect(&addr) {
                s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                s.write_all(&encode_req(1, &[])).unwrap();
                let (res_type, _) = read_response(&mut s).unwrap();
                assert_eq!(res_type, 1);
                conns.push(s);
            }
        }
        assert_eq!(conns.len(), 16);
    }
}

/// Simple deterministic PRNG for fuzzing without external crates.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x0005_1611_7a54 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let val = self.next_u64().to_le_bytes();
            let take = (len - out.len()).min(8);
            out.extend_from_slice(&val[..take]);
        }
        out
    }

    fn next_range(&mut self, min: usize, max: usize) -> usize {
        assert!(min <= max);
        if min == max {
            return min;
        }
        min + (self.next_u64() as usize % (max - min + 1))
    }
}

#[test]
fn test_parser_fuzzing() {
    let root = manifest_dir();
    let server = start_server(&root);
    let addr = format!("127.0.0.1:{}", server.port);

    let mut rng = XorShift64::new(0x2026_1123);
    let iterations = 2500;
    let start_time = std::time::Instant::now();

    println!("[Fuzz] Starting protocol parser fuzzing with {iterations} iterations...");

    let mut handled_errors = 0;
    let mut clean_disconnects = 0;

    for i in 0..iterations {
        let mode = rng.next_range(0, 4);
        let fuzz_payload: Vec<u8> = match mode {
            // Mode 0: Completely random bytes of random length 0..512
            0 => {
                let len = rng.next_range(0, 512);
                rng.next_bytes(len)
            }
            // Mode 1: Length header variations (boundary values)
            1 => {
                let len_choice = match rng.next_range(0, 5) {
                    0 => 0u32,
                    1 => 1u32,
                    2 => 2u32,
                    3 => 4096u32,
                    4 => 4097u32,
                    _ => 0xFFFF_FFFFu32,
                };
                let mut data = len_choice.to_be_bytes().to_vec();
                let body_len = rng.next_range(0, 64);
                data.extend(rng.next_bytes(body_len));
                data
            }
            // Mode 2: Mutation of valid frames
            2 => {
                let mut base = encode_req(1, &[]); // PING
                if rng.next_range(0, 1) == 0 {
                    let key = b"fuzz";
                    let mut put_body = Vec::new();
                    put_body.extend_from_slice(&(key.len() as u16).to_be_bytes());
                    put_body.extend_from_slice(key);
                    put_body.extend_from_slice(&1u16.to_be_bytes());
                    put_body.push(b'v');
                    put_body.push(0);
                    base = encode_req(3, &put_body);
                }
                // Mutate a few bytes
                let mutate_count = rng.next_range(1, 3);
                for _ in 0..mutate_count {
                    if !base.is_empty() {
                        let idx = rng.next_range(0, base.len() - 1);
                        base[idx] = (rng.next_u64() & 0xFF) as u8;
                    }
                }
                base
            }
            // Mode 3: Truncated frame
            3 => {
                let req = encode_req(1, &[]);
                let cut = rng.next_range(1, req.len() - 1);
                req[..cut].to_vec()
            }
            // Mode 4: Invalid versions and ops
            _ => {
                let ver = rng.next_range(0, 255) as u8;
                let op = rng.next_range(0, 255) as u8;
                let body_len = rng.next_range(0, 32);
                let body = rng.next_bytes(body_len);
                let mut f = Vec::new();
                f.extend_from_slice(&((2 + body.len()) as u32).to_be_bytes());
                f.push(ver);
                f.push(op);
                f.extend(body);
                f
            }
        };

        if let Ok(mut stream) = TcpStream::connect(&addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));

            let _ = stream.write_all(&fuzz_payload);
            let mut reply_buf = [0u8; 64];
            match stream.read(&mut reply_buf) {
                Ok(0) => {
                    clean_disconnects += 1;
                }
                Ok(_) => {
                    handled_errors += 1;
                }
                Err(_) => {
                    clean_disconnects += 1;
                }
            }
        }

        // Periodically verify server health every 500 iterations
        if (i + 1) % 500 == 0 {
            let mut health_stream =
                TcpStream::connect(&addr).expect("server must remain alive during fuzzing");
            health_stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            health_stream.write_all(&encode_req(1, &[])).unwrap();
            let (res_type, _) = read_response(&mut health_stream).unwrap();
            assert_eq!(
                res_type, 1,
                "server health check PING failed at iteration {i}"
            );
        }
    }

    let elapsed = start_time.elapsed();
    println!(
        "[Fuzz] Finished {iterations} iterations in {:.2}s. Handled responses: {handled_errors}, Disconnects/Timeouts: {clean_disconnects}. Server healthy.",
        elapsed.as_secs_f64()
    );

    // Final health check
    let mut final_stream =
        TcpStream::connect(&addr).expect("server must be healthy after all fuzzing");
    final_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    final_stream.write_all(&encode_req(1, &[])).unwrap();
    let (res_type, _) = read_response(&mut final_stream).unwrap();
    assert_eq!(res_type, 1, "final server health check PING succeeded");
}
