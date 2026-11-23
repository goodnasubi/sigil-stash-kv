use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

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
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let binary = out.join(format!("tcp-server-{}-{id}", std::process::id()));

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
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let res = reader.read_line(&mut line).map(|_| line);
        let _ = tx.send(res);
    });

    let line = match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(l)) => l,
        Ok(Err(e)) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("failed to read server port from stdout: {e}");
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out waiting for server port on stdout");
        }
    };

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

fn decode_response_bytes(data: &[u8]) -> std::io::Result<(u8, Vec<u8>)> {
    if data.len() < 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "too short for length header",
        ));
    }
    let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if !(2..=4096).contains(&length) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid length {length}"),
        ));
    }
    if data.len() < 4 + length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "payload truncated",
        ));
    }
    let payload = &data[4..4 + length];
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
    let proto_id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let proto_bin = out_dir.join(format!("test-proto-{}-{proto_id}", std::process::id()));
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

    // 11. Fixtures verification (all 16 fixtures)
    {
        let fix_dir = root.join("tests/fixtures");
        assert!(fix_dir.is_dir(), "tests/fixtures directory must exist");

        // 11.1 Valid Request Fixtures
        // req_ping.bin: PING -> SUCCESS (1)
        {
            let data = fs::read(fix_dir.join("req_ping.bin")).expect("read req_ping.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, body) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 1, "req_ping expected SUCCESS (1)");
            assert!(body.is_empty());
        }

        // req_put_no_ttl.bin: PUT key="k", val="v1", no ttl -> SUCCESS (1)
        {
            let data =
                fs::read(fix_dir.join("req_put_no_ttl.bin")).expect("read req_put_no_ttl.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, _) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 1, "req_put_no_ttl expected SUCCESS (1)");
        }

        // req_put_ttl.bin: PUT key="k", val="v1", ttl 60s -> SUCCESS (1)
        {
            let data = fs::read(fix_dir.join("req_put_ttl.bin")).expect("read req_put_ttl.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, _) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 1, "req_put_ttl expected SUCCESS (1)");
        }

        // req_get.bin: GET key="abc" -> NOT_FOUND (3)
        {
            let data = fs::read(fix_dir.join("req_get.bin")).expect("read req_get.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, _) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 3, "req_get for absent key expected NOT_FOUND (3)");
        }

        // req_delete.bin: DELETE key="abc" -> SUCCESS (1)
        {
            let data = fs::read(fix_dir.join("req_delete.bin")).expect("read req_delete.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, _) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 1, "req_delete expected SUCCESS (1)");
        }

        // req_max_frame.bin: length 4096, value 4088 bytes -> ERROR (4), code 4 (ValueLength)
        {
            let data = fs::read(fix_dir.join("req_max_frame.bin")).expect("read req_max_frame.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, body) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 4, "req_max_frame expected ERROR (4)");
            assert_eq!(body[0], 4, "ValueLength error for 4088 byte value");
        }

        // 11.2 Invalid Request Fixtures (Error response or Disconnection)
        // err_length_too_small.bin (len < 2) -> immediate disconnect
        {
            let data = fs::read(fix_dir.join("err_length_too_small.bin"))
                .expect("read err_length_too_small.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let mut buf = [0u8; 1];
            let n = stream.read(&mut buf).unwrap_or(0);
            assert_eq!(n, 0, "err_length_too_small must cause immediate disconnect");
        }

        // err_length_too_large.bin (len > 4096) -> immediate disconnect
        {
            let data = fs::read(fix_dir.join("err_length_too_large.bin"))
                .expect("read err_length_too_large.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let mut buf = [0u8; 1];
            let n = stream.read(&mut buf).unwrap_or(0);
            assert_eq!(n, 0, "err_length_too_large must cause immediate disconnect");
        }

        // err_unsupported_version.bin (ver != 1) -> immediate disconnect
        {
            let data = fs::read(fix_dir.join("err_unsupported_version.bin"))
                .expect("read err_unsupported_version.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let mut buf = [0u8; 1];
            let n = stream.read(&mut buf).unwrap_or(0);
            assert_eq!(
                n, 0,
                "err_unsupported_version must cause immediate disconnect"
            );
        }

        // err_unknown_op.bin (op == 99) -> ERROR (4), code 1 (UnknownOp), connection alive
        {
            let data =
                fs::read(fix_dir.join("err_unknown_op.bin")).expect("read err_unknown_op.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, body) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 4);
            assert_eq!(body[0], 1, "expected UnknownOp error (code 1)");
        }

        // err_truncated_payload.bin -> client closes after truncated payload, server handles cleanly
        {
            let data = fs::read(fix_dir.join("err_truncated_payload.bin"))
                .expect("read err_truncated_payload.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream.write_all(&data).unwrap();
            let _ = stream.shutdown(Shutdown::Both);
        }

        // err_invalid_ttl_tag.bin (ttl_tag == 99) -> ERROR (4), code 2 (InvalidPayload)
        {
            let data = fs::read(fix_dir.join("err_invalid_ttl_tag.bin"))
                .expect("read err_invalid_ttl_tag.bin");
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&data).unwrap();
            let (res_type, body) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 4);
            assert_eq!(body[0], 2, "expected InvalidPayload error (code 2)");
        }

        // 11.3 Response Fixtures Decoding
        // res_success.bin
        {
            let data = fs::read(fix_dir.join("res_success.bin")).expect("read res_success.bin");
            let (res_type, body) = decode_response_bytes(&data).expect("decode res_success");
            assert_eq!(res_type, 1, "res_success type == 1");
            assert!(body.is_empty());
        }

        // res_value.bin
        {
            let data = fs::read(fix_dir.join("res_value.bin")).expect("read res_value.bin");
            let (res_type, body) = decode_response_bytes(&data).expect("decode res_value");
            assert_eq!(res_type, 2, "res_value type == 2");
            let val_len = u16::from_be_bytes([body[0], body[1]]) as usize;
            assert_eq!(val_len, 5);
            assert_eq!(&body[2..2 + val_len], b"world");
        }

        // res_not_found.bin
        {
            let data = fs::read(fix_dir.join("res_not_found.bin")).expect("read res_not_found.bin");
            let (res_type, body) = decode_response_bytes(&data).expect("decode res_not_found");
            assert_eq!(res_type, 3, "res_not_found type == 3");
            assert!(body.is_empty());
        }

        // res_error.bin
        {
            let data = fs::read(fix_dir.join("res_error.bin")).expect("read res_error.bin");
            let (res_type, body) = decode_response_bytes(&data).expect("decode res_error");
            assert_eq!(res_type, 4, "res_error type == 4");
            assert_eq!(body[0], 3, "error code == 3 (KeyLength)");
            let msg_len = u16::from_be_bytes([body[1], body[2]]) as usize;
            assert_eq!(msg_len, 3);
            assert_eq!(&body[3..3 + msg_len], b"bad");
        }
    }

    // 12. Sigil client integration test (native)
    {
        let out_dir = root.join("target");
        let client_id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let client_bin = out_dir.join(format!("test-client-{}-{client_id}", std::process::id()));
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

    // 15. TTL boundaries: 0, -1, i64::MIN, i64::MAX
    {
        for (ttl_val, expected_code) in [
            (0i64, 9u8),      // InvalidTtl
            (-1i64, 9u8),     // InvalidTtl
            (i64::MIN, 9u8),  // InvalidTtl
            (i64::MAX, 10u8), // DeadlineOverflow
        ] {
            let mut stream = TcpStream::connect(&addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut put_body = Vec::new();
            put_body.extend_from_slice(&1u16.to_be_bytes());
            put_body.push(b'k');
            put_body.extend_from_slice(&1u16.to_be_bytes());
            put_body.push(b'v');
            put_body.push(1); // ttl_tag = Seconds
            put_body.extend_from_slice(&ttl_val.to_be_bytes());
            let req = encode_req(3, &put_body);
            stream.write_all(&req).unwrap();
            let (res_type, body) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 4, "expected ERROR (4) for ttl {ttl_val}");
            assert_eq!(
                body[0], expected_code,
                "expected error code {expected_code} for ttl {ttl_val}"
            );

            // Verify connection remains intact and server did not crash
            stream.write_all(&encode_req(1, &[])).unwrap();
            let (res_type, _) = read_response(&mut stream).unwrap();
            assert_eq!(res_type, 1, "subsequent PING after ttl {ttl_val} succeeded");
        }
    }

    // 16. Value length boundaries: 3072 bytes (success) and 3073 bytes (ValueLength error 4)
    {
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        // 3072 bytes value
        let val_3072 = vec![b'x'; 3072];
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&1u16.to_be_bytes());
        put_body.push(b'k');
        put_body.extend_from_slice(&(val_3072.len() as u16).to_be_bytes());
        put_body.extend_from_slice(&val_3072);
        put_body.push(0); // NoExpiry
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, _) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 1, "PUT with 3072-byte value should succeed");

        // 3073 bytes value (frame len = 2 + 3 + 2 + 3073 + 1 = 3081 <= 4096)
        let val_3073 = vec![b'y'; 3073];
        let mut put_body = Vec::new();
        put_body.extend_from_slice(&1u16.to_be_bytes());
        put_body.push(b'k');
        put_body.extend_from_slice(&(val_3073.len() as u16).to_be_bytes());
        put_body.extend_from_slice(&val_3073);
        put_body.push(0); // NoExpiry
        let req = encode_req(3, &put_body);
        stream.write_all(&req).unwrap();
        let (res_type, body) = read_response(&mut stream).unwrap();
        assert_eq!(res_type, 4, "PUT with 3073-byte value should fail");
        assert_eq!(body[0], 4, "expected ValueLength error (code 4)");
    }

    // 17. Capacity limit: max 64 connections, 65th connection is immediately dropped
    {
        let mut conns = Vec::new();
        for _ in 0..64 {
            let mut s = TcpStream::connect(&addr).expect("connect within capacity");
            s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            s.write_all(&encode_req(1, &[])).unwrap();
            let (res_type, _) = read_response(&mut s).unwrap();
            assert_eq!(res_type, 1);
            conns.push(s);
        }
        assert_eq!(conns.len(), 64);

        // 65th connection should be closed immediately by the server
        let mut s65 = TcpStream::connect(&addr).expect("connect 65th");
        s65.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut buf = [0u8; 1];
        let n = s65.read(&mut buf).unwrap_or(0);
        assert_eq!(
            n, 0,
            "65th connection must be dropped immediately due to capacity limit"
        );

        // Drop the 64 connections so capacity is released
        drop(conns);
        thread::sleep(Duration::from_millis(100));

        // Now new connection succeeds
        let mut fresh = TcpStream::connect(&addr).expect("connect after capacity release");
        fresh
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        fresh.write_all(&encode_req(1, &[])).unwrap();
        let (res_type, _) = read_response(&mut fresh).unwrap();
        assert_eq!(res_type, 1, "PING succeeds after capacity is released");
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
