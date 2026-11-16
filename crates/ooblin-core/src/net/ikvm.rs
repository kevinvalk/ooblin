use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{self, ClientRequestBuilder};
use tokio_tungstenite::{WebSocketStream, client_async};

use super::redfish::Connection;
use super::web::Web;
use super::{HUNG, Stream, tls};
use crate::{Error, GOODBYE, Result};

type Socket = WebSocketStream<Stream>;

/// An open iKVM of a Supermicro BMC: the web session its token lives in, and the WebSocket after the handshake.
pub struct Ikvm {
    pub web: Web,
    pub receiver: Receiver,
    pub sender: Sender,
    pub width: u16,
    pub height: u16,
}

/// The BMC's side of the WebSocket, as one stream of bytes.
pub struct Receiver {
    socket: SplitStream<Socket>,
    buffered: Vec<u8>,
}

// IMPORTANT: Once the handshake is done we say nothing but update requests, keys, the pointer and MouseSync. The same
// socket takes power and settings changes, so `send` stays private and we cannot send one of those by mistake.
pub struct Sender {
    socket: SplitSink<Socket, tungstenite::Message>,
}

impl Ikvm {
    /// Logs in to the BMC's web pages for a KVM token, then opens the WebSocket on the web port and shakes hands with
    /// it, which is what authenticates us. We log out again if that fails or hangs.
    pub async fn open(connection: &Connection) -> Result<Self> {
        let web = Web::login(connection).await?;

        let opened = async {
            let token = web.token().await?;
            let socket = socket(connection, web.sid(), "/").await?;
            handshake(socket, &token).await
        };

        match tokio::time::timeout(HUNG, opened).await.unwrap_or(Err(Error::Timeout)) {
            Ok((receiver, sender, [width, height])) => Ok(Self { web, receiver, sender, width, height }),
            Err(error) => {
                web.logout().await;
                Err(error)
            }
        }
    }
}

/// A virtual media WebSocket in `web`'s session, see `supermicro::media`. We give up on it after `HUNG`, like
/// `Ikvm::open` does.
pub async fn media(connection: &Connection, web: &Web) -> Result<(Receiver, Plug)> {
    let socket = tokio::time::timeout(HUNG, socket(connection, web.sid(), "/vm")).await.unwrap_or(Err(Error::Timeout));
    let (sender, receiver) = socket?.split();
    Ok((Receiver { socket: receiver, buffered: Vec::new() }, Plug(Sender { socket: sender })))
}

/// A WebSocket at `path` on the web port, pinned as the web pages are, with the cookie and origin a browser sends.
async fn socket(connection: &Connection, sid: &str, path: &str) -> Result<Socket> {
    let uri: Uri = format!("wss{}{path}", &connection.url["https".len()..]).parse().map_err(websocket)?;
    let stream = tls(uri.host().unwrap_or_default(), uri.port_u16().unwrap_or(443), &connection.pin).await?;
    let request = ClientRequestBuilder::new(uri).with_header("Origin", &connection.url);
    Ok(client_async(request.with_header("Cookie", format!("SID={sid}")), stream).await.map_err(websocket)?.0)
}

/// ATEN's RFB 55.8 handshake. We send each of our parts as its own message, as the viewer does, with the token where
/// RFB has a user name, and ask for a shared screen. Returns the screen's size, a placeholder that the first update
/// corrects.
async fn handshake(socket: Socket, token: &str) -> Result<(Receiver, Sender, [u16; 2])> {
    let (sender, receiver) = socket.split();
    let (mut receiver, mut sender) = (Receiver { socket: receiver, buffered: Vec::new() }, Sender { socket: sender });

    let version = receiver.read(12).await?;
    if !version.starts_with(b"RFB ") {
        return Err(Error::Protocol(format!("not RFB but {}", String::from_utf8_lossy(&version))));
    }
    sender.send(&version).await?;

    let security_type_count = receiver.read(1).await?[0];
    if security_type_count == 0 {
        let reason_length = u32::from_be_bytes(receiver.read(4).await?.try_into().unwrap());
        let reason = receiver.read(reason_length.min(4096) as usize).await?;
        return Err(Error::Protocol(format!("the BMC refused the KVM: {}", String::from_utf8_lossy(&reason))));
    }
    let security_types = receiver.read(security_type_count.into()).await?;
    let Some(security_type) = [16, 15].into_iter().find(|security_type| security_types.contains(security_type)) else {
        return Err(Error::Protocol(format!("none of ATEN's security types in {security_types:?}")));
    };
    sender.send(&[security_type]).await?;

    receiver.read(24).await?; // We ignore the challenge, as the viewer does
    let mut credentials = [0; 48];
    let token = &token.as_bytes()[..token.len().min(24)];
    credentials[..token.len()].copy_from_slice(token);
    sender.send(&credentials).await?;
    if receiver.read(4).await? != [0; 4] {
        return Err(Error::Protocol("the BMC refused the KVM token".into()));
    }

    sender.send(&[1]).await?;
    let server_init = receiver.read(24).await?;
    let name_length = u32::from_be_bytes(server_init[20..24].try_into().unwrap()) as usize;
    if name_length > 4096 {
        return Err(Error::Protocol(format!("a {name_length}-byte screen name")));
    }
    receiver.read(name_length + 12).await?; // We skip the screen's name and ATEN's extension

    let size = [0, 2].map(|offset| u16::from_be_bytes([server_init[offset], server_init[offset + 1]]));
    Ok((receiver, sender, size))
}

impl Receiver {
    /// The next `length` bytes, across WebSocket messages, unless the BMC hung. Dropping the wait loses nothing, as
    /// what arrived stays buffered.
    pub async fn read(&mut self, length: usize) -> Result<Vec<u8>> {
        let fill = async {
            while self.buffered.len() < length {
                match self.socket.next().await {
                    Some(Ok(tungstenite::Message::Binary(bytes))) => self.buffered.extend_from_slice(&bytes),
                    Some(Ok(tungstenite::Message::Close(_))) | None => {
                        return Err(Error::Transport("the BMC hung up".into()));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(websocket(error)),
                }
            }
            Ok(())
        };
        tokio::time::timeout(HUNG, fill).await.unwrap_or(Err(Error::Timeout))?;

        Ok(self.buffered.drain(..length).collect())
    }

    /// Waits for the BMC to close the WebSocket too, within `GOODBYE`, so it is gone before we log out.
    pub async fn closed(&mut self) {
        let _ = tokio::time::timeout(GOODBYE, async { while let Some(Ok(_)) = self.socket.next().await {} }).await;
    }

    /// Drops what is buffered, at least the rest of the WebSocket message we last read from, and returns it.
    pub fn skip(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffered)
    }
}

impl Sender {
    /// Asks for the next update: of what changed, or of the whole screen, `width` by `height`. Returns what it sent.
    pub async fn update(&mut self, is_whole: bool, width: u16, height: u16) -> Result<Vec<u8>> {
        let request = [[3, u8::from(!is_whole)], [0; 2], [0; 2], width.to_be_bytes(), height.to_be_bytes()].concat();
        self.send(&request).await?;
        Ok(request)
    }

    /// A key by its USB HID usage, which ATEN's key message carries where RFB's has a keysym.
    pub async fn key(&mut self, usage: u32, is_down: bool) -> Result<()> {
        self.send(&[&[4, 0, u8::from(is_down), 0, 0][..], &usage.to_be_bytes(), &[0; 9]].concat()).await
    }

    /// The pointer at `x`, `y` on screen with RFB's `buttons` down.
    pub async fn pointer(&mut self, buttons: u8, x: u16, y: u16) -> Result<()> {
        self.send(&[&[5, 0, buttons][..], &x.to_be_bytes(), &y.to_be_bytes(), &[0; 11]].concat()).await
    }

    /// MouseSync, which the viewer sends for every new screen size, so the BMC's pointer catches up with ours.
    pub async fn sync_mouse(&mut self) -> Result<()> {
        self.send(&[7, 7, 0x80]).await
    }

    /// Closes the WebSocket, within `GOODBYE`, after which nothing more goes. See `Receiver::closed`.
    pub async fn close(&mut self) {
        let _ = tokio::time::timeout(GOODBYE, self.socket.close()).await;
    }

    async fn send(&mut self, message: &[u8]) -> Result<()> {
        let sent = self.socket.send(tungstenite::Message::binary(message.to_vec()));

        // We give up on a BMC that hung, because one that stops reading would keep us waiting, and whoever waits for
        // us.
        tokio::time::timeout(HUNG, sent).await.map_err(|_| Error::Timeout)?.map_err(websocket)
    }
}

// Virtual media has a WebSocket of its own, `/vm`, on which we say nothing but its PDUs.
pub struct Plug(Sender);

impl Plug {
    pub async fn send(&mut self, message: &[u8]) -> Result<()> {
        self.0.send(message).await
    }

    pub async fn close(&mut self) {
        self.0.close().await
    }
}

fn websocket(error: impl Into<tungstenite::Error>) -> Error {
    match error.into() {
        tungstenite::Error::Http(response) => Error::Http(response.status().as_u16()),
        error => Error::Transport(error.to_string()),
    }
}
