use std::io;
use std::sync::Arc;

use ooblin_core::amt::{Redirection, RedirectionSession, decode_fields, encode_fields};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::transcript::{Direction, Transcript, received, sent};
use crate::{Amt, REALM, USER, digest};

const NONCE: &str = "mock-amt";
const URI: &str = "/RedirectionService"; // What the Digest is over, as MeshCommander has it

pub async fn serve(listener: TcpListener, tls: TlsAcceptor, amt: Arc<Amt>) {
    while let Ok((tcp, _)) = listener.accept().await {
        let (tls, amt) = (tls.clone(), amt.clone());
        tokio::spawn(async move {
            if let Ok(stream) = tls.accept(tcp).await {
                let _ = session(stream, &amt).await;
            }
        });
    }
}

/// A SOL, KVM or IDER session on 16995. After the handshake (see `accept`) we replay the transcript the fixture has
/// for the session in step with the client, hang up on a client that says something else than was recorded, and then
/// idle until it hangs up. When recording, the device has the session instead (see `record`).
async fn session<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, amt: &Amt) -> io::Result<()> {
    let mut start = [0; 8];
    stream.read_exact(&mut start).await?;
    let (name, session) = match &start[4..] {
        b"SOL " => ("sol", RedirectionSession::Sol),
        b"KVMR" => ("kvm", RedirectionSession::Kvm),
        b"IDER" => ("ider", RedirectionSession::Ider),
        other => return Err(invalid(format!("redirection session {other:?}"))),
    };

    if let Some(device) = &amt.device {
        let opened = Redirection::open(&device.target, device.pin.clone(), session).await;
        let Some(redirection) = accept(&mut stream, opened).await? else { return Ok(()) };
        return record(stream, redirection, session, &mut *device.session.lock().await).await;
    }

    // A fixture without this session behaves like a device that has it disabled.
    let transcript = Transcript::load(&amt.fixture.join(format!("{name}.transcript")));
    let Some(transcript) = accept(&mut stream, transcript).await? else { return Ok(()) };

    let mut sent_count = 0; // How many messages the client sent, as those of SOL say so themselves
    for (direction, data) in transcript.0 {
        match direction {
            // A KVM client sends its input when it likes, so a recorded update request waits for the next one of the
            // client, which has to ask the same. Its input meanwhile goes nowhere.
            Direction::Sent if name == "kvm" && data.first() == Some(&3) => loop {
                match rfb_message(&mut stream).await? {
                    asked if asked == data => break,
                    asked if asked[0] == 3 => return Ok(()),
                    _ => {}
                }
            },
            // An SOL client types what it likes and keeps the session alive in between, so what was typed waits for
            // the client to type something, whatever it is. Its keep-alives go nowhere.
            Direction::Sent if name == "sol" && data.first() == Some(&0x28) => {
                while sol_message(&mut stream, amt, &mut sent_count).await? != 0x28 {}
            }
            Direction::Sent => {
                let mut sent = vec![0; data.len()];
                stream.read_exact(&mut sent).await?;
                if sent != data {
                    return Ok(());
                }
                sent_count += 1;
            }
            // A read of nothing is the device hanging up, so we do that here.
            Direction::Received if data.is_empty() => return Ok(()),
            Direction::Received => send(&mut stream, &data).await?,
        }
    }

    tokio::io::copy(&mut stream, &mut tokio::io::sink()).await.map(drop)
}

/// Starts the session the client asked for, if we have one for it, and hands it over once the client is who it says:
/// it asks what we offer (Digest), sends its user for a challenge, then the response. We do the session start and the
/// Digest over the socket by the specification, taken from `amt-redir-ws-0.1.0.js` of MeshCommander (Apache-2.0).
async fn accept<S: AsyncRead + AsyncWrite + Unpin, T, E>(
    stream: &mut S,
    session: Result<T, E>,
) -> io::Result<Option<T>> {
    let Ok(session) = session else {
        return send(stream, &[0x11, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).await.map(|()| None);
    };

    send(stream, &[0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).await?;

    loop {
        let mut header = [0; 9];
        stream.read_exact(&mut header).await?;
        let mut data = vec![0; u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize];
        stream.read_exact(&mut data).await?;

        let (status, data) = match (header[4], decode_fields(&data).as_slice()) {
            (0, _) => (0, vec![4]),
            (4, [_, _, _, _, _, _, response, _]) if response.is_empty() => (1, encode_fields(&[REALM, NONCE, "auth"])),
            (4, [user, _, nonce, uri, cnonce, nc, response, qop])
                if user == USER && nonce == NONCE && uri == URI && *response == digest(uri, nonce, nc, cnonce, qop) =>
            {
                return reply(stream, 0, &[]).await.map(|()| Some(session));
            }
            _ => return reply(stream, 2, &[]).await.map(|()| None),
        };
        reply(stream, status, &data).await?;
    }
}

/// When recording, the client has its `session` with the `device` until either hangs up, and we keep who said what in
/// `transcript`, a record for each read. What an SOL client says is the exception: that we keep a message each and
/// without its keep-alives, because the replay steps by its messages and need not wait for when it sent those.
async fn record<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    mut device: Redirection,
    session: RedirectionSession,
    transcript: &mut Transcript,
) -> io::Result<()> {
    *transcript = Transcript::default();

    let (mut said_buffer, mut heard_buffer, mut typed) = (vec![0; 64 * 1024], vec![0; 64 * 1024], Vec::new());
    loop {
        tokio::select! {
            read = stream.read(&mut said_buffer) => {
                let said = match read? {
                    0 => return Ok(()),
                    length => &said_buffer[..length],
                };
                device.write_all(said).await.map_err(io::Error::other)?;
                if session != RedirectionSession::Sol {
                    transcript.0.push(sent(said));
                    continue;
                }

                typed.extend(said);
                while let Some(length) = sol_length(&typed).filter(|&length| length <= typed.len()) {
                    let message: Vec<u8> = typed.drain(..length).collect();
                    if message[0] != 0x2B {
                        transcript.0.push(sent(&message));
                    }
                }
            }
            read = device.read(&mut heard_buffer) => {
                let heard = match read.map_err(io::Error::other)? {
                    0 => return Ok(()),
                    length => &heard_buffer[..length],
                };
                send(&mut stream, heard).await?;
                transcript.0.push(received(heard));
            }
        }
    }
}

/// How long the SOL message is that `said` starts with, once enough of it is in to tell: the start of the session, the
/// control lines of the serial port, typing or a keep-alive.
fn sol_length(said: &[u8]) -> Option<usize> {
    Some(match *said.first()? {
        0x20 => 24,
        0x27 => 14,
        0x28 => 10 + usize::from(u16::from_le_bytes([*said.get(8)?, *said.get(9)?])),
        0x2B => 8,
        _ => said.len(),
    })
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, status: u8, data: &[u8]) -> io::Result<()> {
    let header = [&[0x14, status, 0, 0, 4][..], &u32::try_from(data.len()).unwrap().to_le_bytes()].concat();
    send(stream, &[header.as_slice(), data].concat()).await
}

/// Reads an SOL message from the client, which has to be numbered as its `sent_count`th, and returns which it was:
/// typing (0x28), which we log, or a keep-alive (0x2B).
async fn sol_message<S: AsyncRead + Unpin>(stream: &mut S, amt: &Amt, sent_count: &mut u32) -> io::Result<u8> {
    let mut header = [0; 8]; // Its kind, padding and its number
    stream.read_exact(&mut header).await?;

    *sent_count += 1;
    if header[4..] != sent_count.to_le_bytes() {
        return Err(invalid(format!("SOL message {sent_count} numbered {:?}", &header[4..])));
    }

    match header[0] {
        0x28 => {
            let mut typed = vec![0; stream.read_u16_le().await?.into()];
            stream.read_exact(&mut typed).await?;
            amt.requests.lock().unwrap().push(format!("SOL {:?}", String::from_utf8_lossy(&typed)));
        }
        0x2B => {}
        kind => return Err(invalid(format!("SOL message {kind}"))),
    }

    Ok(header[0])
}

/// We flush after every write, because TLS may hold on to what we wrote until we do.
async fn send<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await
}

/// Reads an RFB client message (RFC 6143 7.5), its kind first.
async fn rfb_message<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let kind = stream.read_u8().await?;
    let header_length = match kind {
        0 => 19, // SetPixelFormat
        2 => 3,  // SetEncodings: padding and how many, 4 bytes each
        3 => 9,  // FramebufferUpdateRequest
        4 => 7,  // KeyEvent
        5 => 5,  // PointerEvent
        6 => 7,  // ClientCutText: padding and how long
        _ => return Err(invalid(format!("RFB message {kind}"))),
    };
    let mut message = vec![kind; 1 + header_length];
    stream.read_exact(&mut message[1..]).await?;

    let rest_length = match kind {
        2 => usize::from(u16::from_be_bytes([message[2], message[3]])) * 4,
        6 => u32::from_be_bytes(message[4..8].try_into().unwrap()) as usize,
        _ => 0,
    };
    message.resize(1 + header_length + rest_length, 0);
    stream.read_exact(&mut message[1 + header_length..]).await?;

    Ok(message)
}
