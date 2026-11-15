//! A phone pairing through the bridge is a client of the session server like any
//! other: it attaches to a session, and the server then reports it to the app as
//! a watcher — and, once it types, as the one driving the session
//! (DESIGN.md §14.4). This is the whole relay path over real TLS: the pairing
//! line, the `Remote` declaration the bridge sends, and the frames after it.

use std::{thread, time::Duration};

use kil_remote::{Bridge, Config, QrPayload};
use kil_term::{Client, GridSize, Program, SessionConfig, server};
use rustls::{
    SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

const SIZE: GridSize = GridSize {
    rows: 24,
    cols: 80,
    cell_width: 8,
    cell_height: 16,
};

/// A program that stays up: the session must outlive the test's own attach.
fn long_running() -> SessionConfig {
    let mut config = SessionConfig::new(SIZE);
    config.program = Program::Command {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 60".into()],
    };
    config
}

/// Trusts anything: the test dials a bridge it started itself on loopback.
#[derive(Debug)]
struct AnyCertificate;

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A frame: a `u32` length, a kind byte, then the payload.
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len() + 1)
        .unwrap_or(u32::MAX)
        .to_le_bytes()
        .to_vec();
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

fn text(text: &str) -> Vec<u8> {
    let mut out = u32::try_from(text.len())
        .unwrap_or(u32::MAX)
        .to_le_bytes()
        .to_vec();
    out.extend_from_slice(text.as_bytes());
    out
}

/// The next frame's kind, reading it off a TLS stream.
async fn next_kind(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Option<u8> {
    let mut head = [0u8; 5];
    stream.read_exact(&mut head).await.ok()?;
    let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let mut body = vec![0u8; len - 1];
    stream.read_exact(&mut body).await.ok()?;
    Some(head[4])
}

/// A phone that pairs, attaches to `id`, and holds the connection open.
struct Phone {
    stream: tokio_rustls::client::TlsStream<TcpStream>,
}

impl Phone {
    async fn dial(host: &str, port: u16, code: &str, id: &str) -> Option<Self> {
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(AnyCertificate))
            .with_no_client_auth();
        let connector = TlsConnector::from(std::sync::Arc::new(config));
        let tcp = TcpStream::connect((host, port)).await.ok()?;
        let name = ServerName::try_from("kil").ok()?;
        let mut stream = connector.connect(name, tcp).await.ok()?;
        stream
            .write_all(format!("KIL1 {code}\n").as_bytes())
            .await
            .ok()?;
        let mut line = String::new();
        let mut reader = BufReader::new(stream);
        AsyncBufReadExt::read_line(&mut reader, &mut line)
            .await
            .ok()?;
        assert!(
            line.starts_with("OK "),
            "the bridge paired the phone: {line}"
        );
        let mut stream = reader.into_inner();
        stream
            .write_all(&frame(1, &2u32.to_le_bytes()))
            .await
            .ok()?;
        assert_eq!(next_kind(&mut stream).await, Some(1), "hello is answered");
        stream.write_all(&frame(5, &text(id))).await.ok()?;
        assert_eq!(next_kind(&mut stream).await, Some(11), "attached");
        Some(Self { stream })
    }
}

/// Waits for `read` to see `want`, the way the app's 2 s presence poll would.
fn until(want: &[&str], read: impl Fn() -> Result<Vec<String>, std::io::Error>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if read().is_ok_and(|seen| seen == want) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server reports {want:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn a_phone_through_the_bridge_is_reported_watching_and_driving() {
    let dir = std::env::temp_dir().join(format!("kil-phone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    server::serve_in_thread(&dir).unwrap();
    let client = Client::connect(&dir).unwrap();
    // The app's pane: one client of its own, like every pane in the window.
    let _pane = client
        .spawn("kil-bridge-1", &long_running(), |_| {})
        .unwrap();
    assert_eq!(client.sessions().unwrap(), ["kil-bridge-1"]);

    let bridge = Bridge::start(&Config {
        dir: dir.clone(),
        port: 0,
        host: Some("127.0.0.1".to_owned()),
        launcher: None,
        public: false,
        tunnel_hostname: None,
        tunnel_token: None,
        reset_pairing: false,
    })
    .unwrap();
    let qr = {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(qr) = bridge.qr() {
                break qr;
            }
            assert!(std::time::Instant::now() < deadline, "the QR becomes ready");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };

    // Nobody is on the session yet: no marks.
    until(&[], || client.watchers());
    until(&[], || client.controlling());

    let mut phone = Phone::dial("127.0.0.1", qr.port, &qr.code, "kil-bridge-1")
        .await
        .expect("the phone attaches through the bridge");
    // Attached without typing: watching, never driving.
    until(&["kil-bridge-1"], || client.watchers());
    until(&[], || client.controlling());

    // It types: the session reads as driven, not just watched.
    phone
        .stream
        .write_all(&frame(6, b"echo hi\n"))
        .await
        .unwrap();
    until(&["kil-bridge-1"], || client.controlling());
    until(&["kil-bridge-1"], || client.watchers());

    drop(phone);
    until(&[], || client.watchers());
    bridge.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

type TlsReader = BufReader<tokio_rustls::client::TlsStream<TcpStream>>;

async fn tls_connect(port: u16) -> Option<TlsReader> {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AnyCertificate))
        .with_no_client_auth();
    let connector = TlsConnector::from(std::sync::Arc::new(config));
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
    let stream = connector
        .connect(ServerName::try_from("kil").ok()?, tcp)
        .await
        .ok()?;
    Some(BufReader::new(stream))
}

async fn say(reader: &mut TlsReader, line: &str) -> Option<String> {
    reader
        .get_mut()
        .write_all(format!("{line}\n").as_bytes())
        .await
        .ok()?;
    let mut reply = String::new();
    reader.read_line(&mut reply).await.ok()?;
    Some(reply.trim().to_owned())
}

fn hex32(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The phone-side KIL3 answer, computed the way the spec defines it.
fn kil3_answer(token_hex: &str, nonce_hex: &str, fingerprint_hex: &str) -> String {
    let key_bytes = ring::digest::digest(&ring::digest::SHA256, &hex32(token_hex));
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key_bytes.as_ref());
    let mut message = b"kil-auth-v3".to_vec();
    message.push(0);
    message.extend_from_slice(&hex32(nonce_hex));
    message.push(0);
    message.extend_from_slice(&hex32(fingerprint_hex));
    to_hex(ring::hmac::sign(&key, &message).as_ref())
}

fn bridge_config(dir: &std::path::Path, reset_pairing: bool) -> Config {
    Config {
        dir: dir.to_owned(),
        port: 0,
        host: Some("127.0.0.1".to_owned()),
        launcher: None,
        public: false,
        tunnel_hostname: None,
        tunnel_token: None,
        reset_pairing,
    }
}

fn wait_for_pairing_qr(bridge: &Bridge) -> QrPayload {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(qr) = bridge.qr() {
            return qr;
        }
        assert!(std::time::Instant::now() < deadline, "the QR becomes ready");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Accepts the bridge's startup connections; aborting it closes the listener
/// and every accepted socket.
fn stoppable_session_server(dir: &std::path::Path) -> std::io::Result<tokio::task::JoinHandle<()>> {
    std::fs::create_dir_all(dir)?;
    let listener = tokio::net::UnixListener::bind(server::socket_path(dir))?;
    Ok(tokio::spawn(async move {
        let mut connections = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            connections.push(stream);
        }
    }))
}

#[tokio::test]
async fn kil1_delivers_the_token_when_the_session_server_stops_after_qr() {
    let dir = std::env::temp_dir().join(format!("kil-pairing-relay-down-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let session = stoppable_session_server(&dir).unwrap();
    let bridge = Bridge::start(&bridge_config(&dir, false)).unwrap();
    let qr = wait_for_pairing_qr(&bridge);

    session.abort();
    let _ = session.await;
    std::fs::remove_file(server::socket_path(&dir)).unwrap();
    let mut conn = tls_connect(qr.port).await.expect("connect");
    let reply = say(&mut conn, &format!("KIL1 {}", qr.code))
        .await
        .expect("the token arrives even though the relay cannot connect");
    let token = reply.strip_prefix("OK ").expect("KIL1 pairs");
    assert_eq!(token.len(), 64);
    assert!(bridge.paired());

    drop(conn);
    bridge.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn pairing_survives_restarts_and_reset_revokes_it() {
    let dir = std::env::temp_dir().join(format!("kil-pairing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    server::serve_in_thread(&dir).unwrap();

    // First run: pair with the code, keep the issued token.
    let first = Bridge::start(&bridge_config(&dir, false)).unwrap();
    let qr = wait_for_pairing_qr(&first);
    assert_eq!(qr.code.len(), 64, "256-bit pairing code");
    assert!(!first.paired());
    let mut conn = tls_connect(qr.port).await.expect("connect");
    let reply = say(&mut conn, &format!("KIL1 {}", qr.code))
        .await
        .expect("reply");
    let token = reply.strip_prefix("OK ").expect("KIL1 pairs").to_owned();
    assert_eq!(token.len(), 64);
    drop(conn);
    assert!(first.paired());
    let fingerprint = qr.fingerprint.clone();
    let code = qr.code.clone();
    first.stop();

    // Restart on the same dir: same identity/code, already paired, and both
    // KIL2 and KIL3 accept the old token while a wrong KIL3 answer fails.
    let second = Bridge::start(&bridge_config(&dir, false)).unwrap();
    let qr = wait_for_pairing_qr(&second);
    assert_eq!(qr.fingerprint, fingerprint);
    assert_eq!(qr.code, code);
    assert!(second.paired(), "paired before any connection");
    let mut spent = tls_connect(qr.port).await.expect("connect");
    assert_eq!(
        say(&mut spent, &format!("KIL1 {code}")).await.as_deref(),
        Some("ERR rejected"),
        "the persisted verifier keeps the QR code spent after a restart"
    );
    drop(spent);
    let mut conn = tls_connect(qr.port).await.expect("connect");
    assert_eq!(
        say(&mut conn, &format!("KIL2 {token}")).await.as_deref(),
        Some(format!("OK {token}").as_str()),
        "legacy clients receive the same OK payload as before KIL3"
    );
    drop(conn);
    let mut conn = tls_connect(qr.port).await.expect("connect");
    let chal = say(&mut conn, "KIL3").await.expect("reply");
    let nonce = chal.strip_prefix("CHAL ").expect("KIL3 challenges");
    assert_eq!(nonce.len(), 64);
    let answer = kil3_answer(&token, nonce, &fingerprint);
    // Pipeline Hello with RESP: the bridge's buffered line reader must hand
    // the already-read frame bytes to the relay rather than discarding them.
    let mut outgoing = format!("RESP {answer}\n").into_bytes();
    outgoing.extend(frame(1, &2u32.to_le_bytes()));
    conn.get_mut().write_all(&outgoing).await.expect("respond");
    let mut reply = String::new();
    conn.read_line(&mut reply).await.expect("read OK");
    assert_eq!(reply.trim(), "OK device");
    assert_eq!(
        next_kind(conn.get_mut()).await,
        Some(1),
        "the pipelined Hello reaches the session server"
    );
    drop(conn);
    let mut conn = tls_connect(qr.port).await.expect("connect");
    let chal = say(&mut conn, "KIL3").await.expect("reply");
    assert!(chal.starts_with("CHAL "));
    let wrong = "00".repeat(32);
    assert_eq!(
        say(&mut conn, &format!("RESP {wrong}")).await.as_deref(),
        Some("ERR rejected")
    );
    drop(conn);
    second.stop();

    // Reset: fresh identity and code; the old token no longer authenticates.
    let third = Bridge::start(&bridge_config(&dir, true)).unwrap();
    let qr = wait_for_pairing_qr(&third);
    assert_ne!(qr.fingerprint, fingerprint);
    assert_ne!(qr.code, code);
    assert!(!third.paired());
    let mut conn = tls_connect(qr.port).await.expect("connect");
    assert_eq!(
        say(&mut conn, &format!("KIL2 {token}")).await.as_deref(),
        Some("ERR rejected")
    );
    drop(conn);
    let mut conn = tls_connect(qr.port).await.expect("connect");
    assert_eq!(
        say(&mut conn, &format!("KIL1 {code}")).await.as_deref(),
        Some("ERR rejected")
    );
    drop(conn);
    let mut conn = tls_connect(qr.port).await.expect("connect");
    let reply = say(&mut conn, &format!("KIL1 {}", qr.code))
        .await
        .expect("the reset permits a new pairing");
    let new_token = reply.strip_prefix("OK ").expect("new device pairs");
    assert_eq!(new_token.len(), 64);
    assert_ne!(new_token, token);
    drop(conn);
    third.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_tls_cap_includes_connections_waiting_for_authentication() {
    let dir = std::env::temp_dir().join(format!("kil-concurrency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    server::serve_in_thread(&dir).unwrap();
    let bridge = Bridge::start(&bridge_config(&dir, false)).unwrap();
    let qr = {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(qr) = bridge.qr() {
                break qr;
            }
            assert!(std::time::Instant::now() < deadline, "the QR becomes ready");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };

    // None of these completes TLS: all sixteen slots are therefore held in
    // accept/auth waits, not merely by fully established relay sessions.
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(TcpStream::connect(("127.0.0.1", qr.port)).await.unwrap());
    }
    let mut extra = TcpStream::connect(("127.0.0.1", qr.port)).await.unwrap();
    let mut byte = [0u8; 1];
    let rejected = tokio::time::timeout(Duration::from_secs(2), extra.read(&mut byte))
        .await
        .expect("the seventeenth socket is rejected promptly");
    assert!(
        matches!(rejected, Ok(0) | Err(_)),
        "the connection over the cap is closed"
    );

    drop(held);
    bridge.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
