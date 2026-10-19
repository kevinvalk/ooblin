//! The SOL, KVM and IDER sessions on 16995. The session start and Digest over the socket we do by the spec, from
//! MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0). After that we replay the fixture's transcript for the session in
//! step with the client, hanging up on one that says something else than was recorded, then idle until it hangs up. A
//! KVM client asks for updates when it likes and sends its input in between, so there we step by RFB message, and an
//! SOL client types when it likes and keeps the session alive in between, so there we step by what it types.

use std::path::{Path, PathBuf};

use amt_core::{decode_fields, encode_fields};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::transcript::{Direction, Transcript};
use crate::{REALM, USER, digest};

const NONCE: &str = "mock-amt";

pub async fn serve(listener: TcpListener, tls: TlsAcceptor, fixture: PathBuf) {
    while let Ok((tcp, _)) = listener.accept().await {
        let (tls, fixture) = (tls.clone(), fixture.clone());
        tokio::spawn(async move {
            if let Ok(stream) = tls.accept(tcp).await {
                let _ = session(stream, &fixture).await;
            }
        });
    }
}

async fn session<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, fixture: &Path) -> std::io::Result<()> {
    let mut start = [0; 8];
    stream.read_exact(&mut start).await?;
    let name = match &start[4..] {
        b"SOL " => "sol",
        b"KVMR" => "kvm",
        b"IDER" => "ider",
        other => panic!("unknown redirection session {other:?}"),
    };
    // A fixture without this session behaves like a device with it disabled.
    let Ok(transcript) = Transcript::load(&fixture.join(format!("{name}.transcript"))) else {
        return send(&mut stream, &[0x11, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).await;
    };
    send(&mut stream, &[0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).await?;

    // Authentication: the client asks what we offer (Digest), sends its user for a challenge, then the response.
    loop {
        let mut header = [0; 9];
        stream.read_exact(&mut header).await?;
        let mut data = vec![0; u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize];
        stream.read_exact(&mut data).await?;
        let (status, data) = match (header[4], decode_fields(&data).as_slice()) {
            (0, _) => (0, vec![4]),
            (4, [_, _, _, _, _, _, response, _]) if response.is_empty() => (1, encode_fields(&[REALM, NONCE, "auth"])),
            (4, [user, _, nonce, uri, cnonce, nc, response, qop])
                if user == USER && nonce == NONCE && *response == digest(uri, nonce, nc, cnonce, qop) =>
            {
                break reply(&mut stream, 0, &[]).await?;
            }
            _ => return reply(&mut stream, 2, &[]).await,
        };
        reply(&mut stream, status, &data).await?;
    }

    for (direction, data) in transcript.0 {
        match direction {
            // A recorded SetEncodings or update request waits for the client's next one, its input goes nowhere.
            Direction::Sent if name == "kvm" && matches!(data[0], 2 | 3) => {
                while rfb_message(&mut stream).await? != data[0] {}
            }
            // What was typed waits for the client to type something, its keep-alives go nowhere.
            Direction::Sent if name == "sol" && data[0] == 0x28 => while sol_message(&mut stream).await? != 0x28 {},
            Direction::Sent => {
                let mut sent = vec![0; data.len()];
                stream.read_exact(&mut sent).await?;
                if sent != data {
                    return Ok(());
                }
            }
            Direction::Received => send(&mut stream, &data).await?,
        }
    }
    tokio::io::copy(&mut stream, &mut tokio::io::sink()).await.map(drop)
}

async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, status: u8, data: &[u8]) -> std::io::Result<()> {
    let header = [&[0x14, status, 0, 0, 4][..], &u32::try_from(data.len()).unwrap().to_le_bytes()].concat();
    send(stream, &[header.as_slice(), data].concat()).await
}

/// Reads an SOL message from the client and says which it was: typing (0x28) or a keep-alive (0x2B).
async fn sol_message<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<u8> {
    let mut header = [0; 8]; // its kind, padding and its sequence
    stream.read_exact(&mut header).await?;
    match header[0] {
        0x28 => {
            let len = stream.read_u16_le().await?;
            stream.read_exact(&mut vec![0; len.into()]).await?;
        }
        0x2B => {}
        kind => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("SOL message {kind}"))),
    }
    Ok(header[0])
}

/// TLS may hold on to what we write until we flush.
async fn send<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await
}

/// Reads an RFB client message (RFC 6143 7.5) and says which it was.
async fn rfb_message<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<u8> {
    let kind = stream.read_u8().await?;
    let mut header = [0; 3]; // padding, and SetEncodings' count
    let len = match kind {
        0 => 19, // SetPixelFormat
        2 => {
            stream.read_exact(&mut header).await?;
            usize::from(u16::from_be_bytes([header[1], header[2]])) * 4
        }
        3 => 9, // FramebufferUpdateRequest
        4 => 7, // KeyEvent
        5 => 5, // PointerEvent
        6 => {
            // ClientCutText
            stream.read_exact(&mut header).await?;
            stream.read_u32().await? as usize
        }
        _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("RFB message {kind}"))),
    };
    stream.read_exact(&mut vec![0; len]).await?;
    Ok(kind)
}
