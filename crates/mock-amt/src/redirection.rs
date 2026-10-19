//! The SOL, KVM and IDER sessions on 16995. The session start and Digest over the socket we do by the spec, from
//! MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0). After that we replay the fixture's transcript for the session in
//! step with the client, then idle until it hangs up.

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
        return stream.write_all(&[0x11, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).await;
    };
    stream.write_all(&[0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).await?;

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
            Direction::Sent => stream.read_exact(&mut vec![0; data.len()]).await.map(drop)?,
            Direction::Received => stream.write_all(&data).await?,
        }
    }
    tokio::io::copy(&mut stream, &mut tokio::io::sink()).await.map(drop)
}

async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, status: u8, data: &[u8]) -> std::io::Result<()> {
    let header = [&[0x14, status, 0, 0, 4][..], &u32::try_from(data.len()).unwrap().to_le_bytes()].concat();
    stream.write_all(&[header.as_slice(), data].concat()).await
}
