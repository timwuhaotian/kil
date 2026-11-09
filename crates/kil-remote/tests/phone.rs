//! A phone pairing through the bridge is a client of the session server like any
//! other: it attaches to a session, and the server then reports it to the app as
//! a watcher — and, once it types, as the one driving the session
//! (DESIGN.md §14.4). This is the whole relay path over real TLS: the pairing
//! line, the `Remote` declaration the bridge sends, and the frames after it.

use std::{thread, time::Duration};

use kil_remote::{Bridge, Config};
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
