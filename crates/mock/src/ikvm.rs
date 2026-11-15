//! The BMC's web session and KVM: logging in for a token, the WebSocket and ATEN's handshake on it, then the fixture's
//! `kvm.transcript` in step with what's asked; and recording one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, ensure};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, COOKIE, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SET_COOKIE, UPGRADE};
use hyper::upgrade::Upgraded;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ooblin_core::Target;
use ooblin_core::supermicro::{Connection, Ikvm};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::transcript::{Direction, Transcript};
use crate::{Bmc, PASSWORD, USER};

/// The KVM half's own: web sessions, and what to refuse.
#[derive(Default)]
pub(crate) struct State {
    /// Whether each login so far, by number, is still logged in: its SID and KVM token are the number.
    sessions: std::sync::Mutex<Vec<bool>>,
    refuse: AtomicBool,
    refuse_web: AtomicBool,
    hang_up: AtomicBool,
    /// Ticks on every logout, for the screens to look whether theirs was it.
    logouts: watch::Sender<()>,
    /// The CGI's answers changed since we started, by read (`GETPORTSINFO.XML`): the fixture's `cgi/` holds the rest.
    changed: std::sync::Mutex<HashMap<String, String>>,
}

type Socket = WebSocketStream<TokioIo<Upgraded>>;

pub(crate) async fn serve(bmc: &Arc<Bmc>, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let path = request.uri().path_and_query().map_or("", |path| path.as_str()).to_owned();
    let websocket = request.headers().contains_key(SEC_WEBSOCKET_KEY);
    log(bmc, format!("{} {path}", if websocket { "WS" } else { request.method().as_str() }));
    let session =
        request.headers().get(COOKIE).and_then(|cookie| cookie.to_str().ok()?.strip_prefix("SID=")?.parse().ok());
    let session = session.filter(|&session| bmc.ikvm.live(session));
    let response = Response::builder();
    let response = match (request.method(), path.as_str()) {
        // The X10 takes the upgrade without its cookie: the token in the handshake is what counts.
        (&Method::GET, "/" | "/vm") if websocket => {
            let accept = derive_accept_key(request.headers()[SEC_WEBSOCKET_KEY].as_bytes());
            let bmc = bmc.clone();
            tokio::spawn(async move {
                let Ok(upgraded) = hyper::upgrade::on(request).await else { return };
                let mut socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
                let _ = match path.as_str() {
                    "/" => screen(&bmc, &mut socket).await,
                    _ => media(&bmc, &mut socket).await,
                };
                // Before it's closed, so it's logged before what the client does next.
                log(&bmc, "WS closed".into());
                let _ = socket.close(None).await;
            });
            let response = response.status(StatusCode::SWITCHING_PROTOCOLS).header(CONNECTION, "Upgrade");
            response.header(UPGRADE, "websocket").header(SEC_WEBSOCKET_ACCEPT, accept).body(Full::default())
        }
        (&Method::POST, "/cgi/login.cgi") => {
            let form = request.into_body().collect().await.map(|body| body.to_bytes()).unwrap_or_default();
            let form: HashMap<_, _> = form_urlencoded::parse(&form).collect();
            let known =
                form.get("name").is_some_and(|name| name == USER) && form.get("pwd").is_some_and(|pwd| pwd == PASSWORD);
            if known && !bmc.ikvm.refuse_web.load(Ordering::SeqCst) {
                let mut sessions = bmc.ikvm.sessions.lock().unwrap();
                sessions.push(true);
                let cookie = format!("SID={:015}; path=/; secure; HttpOnly", sessions.len() - 1);
                response.header(SET_COOKIE, cookie).body(Full::from("<html>mainmenu</html>"))
            } else {
                response.body(Full::from("<html>login_alert</html>"))
            }
        }
        (&Method::GET, "/cgi/url_redirect.cgi?url_name=man_ikvm_html5_bootstrap") => {
            let token =
                session.map(|session| format!(r#"<input type="hidden" id="entry_value" value="{session:024}">"#));
            response.body(Full::from(format!("<html>{}</html>", token.unwrap_or_default())))
        }
        // The dashboard, for its token against cross-site requests, which its CGI wants.
        (&Method::GET, "/cgi/url_redirect.cgi?url_name=sys_info") => {
            let token =
                session.map(|session| format!(r#"<script>SmcCsrfInsert ("CSRF_TOKEN", "{session:08}");</script>"#));
            response.body(Full::from(format!("<html>{}</html>", token.unwrap_or_default())))
        }
        // Its CGI's reads, from the fixture's `cgi/`: `GENERIC_INFO.XML=(0,0)`, as its pages ask.
        (&Method::POST, "/cgi/ipmi.cgi") => {
            let token = request.headers().get("CSRF_TOKEN").and_then(|token| token.to_str().ok()?.parse().ok());
            let body = request.into_body().collect().await.map(|body| body.to_bytes()).unwrap_or_default();
            let body = String::from_utf8_lossy(&body);
            let read = body.strip_suffix("=(0,0)").unwrap_or_default();
            log(bmc, format!("CGI {read}"));
            let allowed = session.is_some() && token == session && !read.contains('/');
            let changed = bmc.ikvm.changed.lock().unwrap().get(read).cloned();
            let xml = changed.or_else(|| std::fs::read_to_string(bmc.fixture.join("cgi").join(read)).ok());
            match xml.filter(|_| allowed) {
                Some(xml) => response.body(Full::from(xml)),
                None => response.status(StatusCode::FORBIDDEN).body(Full::default()),
            }
        }
        // Its CGI's changes we make, the port page's and the network page's: each all at once, which then reads back.
        (&Method::POST, "/cgi/op.cgi") => {
            let token = request.headers().get("CSRF_TOKEN").and_then(|token| token.to_str().ok()?.parse().ok());
            let form = request.into_body().collect().await.map(|body| body.to_bytes()).unwrap_or_default();
            let form: Vec<(String, String)> = form_urlencoded::parse(&form).into_owned().collect();
            log(
                bmc,
                format!(
                    "CGI {}",
                    form.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("&")
                ),
            );
            let op = form.iter().find(|(name, _)| name == "op").map(|(_, op)| op.as_str());
            let answer = match op {
                Some("config_port") => config_port(bmc, &form),
                Some("config_lan") => config_lan(bmc, &form),
                _ => None,
            };
            match answer.filter(|_| session.is_some() && token == session) {
                Some(answer) => response.body(Full::from(answer)),
                None => response.status(StatusCode::FORBIDDEN).body(Full::default()),
            }
        }
        (&Method::GET, "/cgi/logout.cgi") => {
            if let Some(session) = session {
                bmc.ikvm.sessions.lock().unwrap()[session] = false;
                bmc.ikvm.logouts.send_replace(());
            }
            response.body(Full::default())
        }
        _ => response.status(StatusCode::NOT_FOUND).body(Full::default()),
    };
    response.unwrap()
}

/// The port page's change: every port and service at once, as the CGI and Redfish read them then. None of it if a port
/// is 5120, which we make the BMC keep for itself.
fn config_port(bmc: &Bmc, form: &[(String, String)]) -> Option<String> {
    if let Some((name, _)) = form.iter().find(|(name, port)| name.ends_with("_PORT") && port == "5120") {
        return Some(format!(r#"<?xml version="1.0"?><IPMI><PORTCONF_STATUS {name}_STATUS="1"/></IPMI>"#));
    }
    let fields = form.iter().filter(|(name, _)| name != "op");
    let attributes: String = fields.map(|(name, value)| format!(" {name}=\"{value}\"")).collect();
    let ports = format!("<?xml version=\"1.0\"?>\n<IPMI>\n  <PORT_INFO{attributes}/>\n</IPMI>\n");
    bmc.ikvm.changed.lock().unwrap().insert("GETPORTSINFO.XML".into(), ports);
    let field = |name: String| form.iter().find(|(field, _)| *field == name).map(|(_, value)| value.clone());
    bmc.change_redfish("redfish/v1/Managers/1/NetworkProtocol", |redfish| {
        for (name, key) in
            [("HTTP", "HTTP"), ("SSH", "SSH"), ("KVMIP", "IKVM"), ("VirtualMedia", "VM"), ("SNMP", "SNMP")]
        {
            if let (Some(on), Some(port)) = (field(format!("{key}_SERVICE")), field(format!("{key}_PORT"))) {
                redfish[name] =
                    serde_json::json!({"ProtocolEnabled": on == "1", "Port": port.parse::<u16>().unwrap_or(0)});
            }
        }
    });
    Some(
        r#"<?xml version="1.0"?><IPMI><PORTCONF_STATUS HTTP_PORT_STATUS="0" HTTPS_PORT_IN_USE_STATUS="0"/></IPMI>"#
            .into(),
    )
}

/// The network page's change, as Redfish reads it then. It wants every field its page sends.
fn config_lan(bmc: &Bmc, form: &[(String, String)]) -> Option<String> {
    let field = |name: &str| form.iter().find(|(field, _)| field == name).map(|(_, value)| value.clone());
    let names = ["bmcip", "bmcmask", "gatewayip", "en_dhcp", "en_vlan", "vlanID", "rmcpport", "dns_server"];
    let names = names.iter().chain(&["bmcipv6_dns_server", "bmcipv6_opt", "bmcipv6_autoconf", "dhcpv6_mode"]);
    if !names.chain(&["lan_interface", "link_conf", "hostname"]).all(|name| field(name).is_some()) {
        return None;
    }
    bmc.change_redfish("redfish/v1/Managers/1/EthernetInterfaces/1", |redfish| {
        let dhcp = field("en_dhcp").as_deref() == Some("on");
        redfish["HostName"] = field("hostname").into();
        redfish["IPv4Addresses"] = serde_json::json!([{
            "Address": field("bmcip"), "SubnetMask": field("bmcmask"), "Gateway": field("gatewayip"),
            "AddressOrigin": if dhcp { "DHCP" } else { "Static" },
        }]);
        redfish["NameServers"] = serde_json::json!([field("dns_server")]);
        let vlan = field("vlanID").and_then(|id| id.parse::<u16>().ok()).unwrap_or(0);
        redfish["VLAN"] = serde_json::json!({"VLANEnable": field("en_vlan").as_deref() == Some("on"), "VLANId": vlan});
    });
    Some("<html></html>".into())
}

impl Bmc {
    /// Ends every web session, as someone logging out in the BMC's own pages does, and with them their screens.
    pub fn log_out(&self) {
        self.ikvm.sessions.lock().unwrap().fill(false);
        self.ikvm.logouts.send_replace(());
    }

    /// Refuses the next KVM token, as the BMC does a dead one.
    pub fn refuse_kvm(&self) {
        self.ikvm.refuse.store(true, Ordering::SeqCst);
    }

    /// Refuses every web login from now on, the right password too, while Redfish goes on taking it.
    pub fn refuse_web(&self) {
        self.ikvm.refuse_web.store(true, Ordering::SeqCst);
    }

    /// Hangs up on a drive plugged out, where the X10 says it's out.
    pub fn hang_up_on_plug_out(&self) {
        self.ikvm.hang_up.store(true, Ordering::SeqCst);
    }

    /// Answers the CGI's `read` (`CONFIG_INFO.XML`) with `xml` from now on.
    pub fn set_cgi(&self, read: &str, xml: &str) {
        self.ikvm.changed.lock().unwrap().insert(read.into(), xml.into());
    }
}

impl State {
    fn live(&self, session: usize) -> bool {
        self.sessions.lock().unwrap().get(session) == Some(&true)
    }
}

fn log(bmc: &Bmc, line: String) {
    bmc.requests.lock().unwrap().push(line);
}

/// ATEN's handshake with the X10's values, then the transcript in step: what we sent waits for the client's next
/// update request, each of what the BMC sent is a message. After it, each request is answered with the X10's "nothing
/// changed", no sooner than the X10 does. The client says nothing but update requests, keys, the pointer and MouseSync,
/// else we hang up, and its web session's logout ends the screen.
async fn screen(bmc: &Bmc, socket: &mut Socket) -> anyhow::Result<()> {
    send(socket, b"RFB 055.008\n").await?;
    ensure!(receive(socket).await? == b"RFB 055.008\n", "not ATEN's version");
    send(socket, &[1, 16]).await?;
    ensure!(receive(socket).await? == [16], "not ATEN's security");
    send(socket, &[0; 24]).await?;
    let credentials = receive(socket).await?;
    ensure!(credentials.len() == 48 && credentials[24..] == [0; 24], "not ATEN's credentials");
    let session = std::str::from_utf8(&credentials[..24]).ok().and_then(|token| token.parse().ok());
    let refused = bmc.ikvm.refuse.swap(false, Ordering::SeqCst);
    let Some(session) = session.filter(|&session| bmc.ikvm.live(session) && !refused) else {
        bail!("a token we don't know");
    };
    send(socket, &[0; 4]).await?;
    ensure!(receive(socket).await? == [1], "not a shared screen");
    let name = b"ATEN iKVM Server";
    let format = [0x20, 0x18, 0, 1, 0, 0xFF, 0, 0xFF, 0, 0xFF, 0x10, 8, 0, 0, 0, 0];
    let extension = [0, 0, 0, 0, 0x41, 0xEF, 0xF4, 0x50, 1, 1, 1, 1];
    let size = [480u16.to_be_bytes(), 640u16.to_be_bytes()].concat();
    send(socket, &[&size[..], &format, &(name.len() as u32).to_be_bytes(), name, &extension].concat()).await?;

    let mut logouts = bmc.ikvm.logouts.subscribe();
    let Transcript(records) = Transcript::load(&bmc.fixture.join("kvm.transcript")).unwrap_or_default();
    for (direction, data) in records {
        match direction {
            Direction::Sent => drop(request(bmc, socket, &mut logouts, session).await?),
            Direction::Received => send(socket, &data).await?,
        }
    }
    let mut last = Instant::now();
    loop {
        let request = request(bmc, socket, &mut logouts, session).await?;
        tokio::time::sleep_until(last + Duration::from_millis(40)).await;
        let header = [&[0, 0, 0, 1, 0, 0, 0, 0][..], &request[6..10], &[0, 0, 0, 87, 0, 0, 0, 0, 0, 0, 0, 16]].concat();
        send(socket, &[&header[..], &[4, 7, 1, 0xA6, 0, 0, 0, 0x90], &[0; 8]].concat()).await?;
        last = Instant::now();
    }
}

/// Virtual media, as the X10's: its slots, all free, on a socket of their own, or a CD-ROM drive plugged in with a live
/// session's token. In that one we ask what a host first does (INQUIRY, READ CAPACITY) and read the second block,
/// logging what came back with success (`vm 28 0101…`, the first 8 bytes), till it's plugged out, which may come any
/// time. A token `refuse_kvm` refuses is refused until the client hangs up, anything else hangs up.
async fn media(bmc: &Bmc, socket: &mut Socket) -> anyhow::Result<()> {
    let pdu = |class: [u8; 4], body: &[u8]| [&class[..], &(body.len() as u32).to_le_bytes(), body].concat();
    let plug_in = receive(socket).await?;
    if plug_in == pdu([0, 0, 0, 8], &[]) {
        return send(socket, &pdu([0, 0, 0, 8], &[0, 255, 255, 255])).await;
    }
    ensure!(plug_in.len() > 52 && plug_in[..8] == [0, 0, 0, 1, 44, 0, 0, 0], "not a plug-in");
    let token = String::from_utf8_lossy(&[&plug_in[8..23], &plug_in[24..33]].concat()).into_owned();
    let session = token.parse().ok().filter(|&session| bmc.ikvm.live(session));
    ensure!(session.is_some() && plug_in[48..50] == [0xC3, 3], "not a CD-ROM in the first slot by a live session");
    if bmc.ikvm.refuse.swap(false, Ordering::SeqCst) {
        send(socket, &pdu([0, 0, 0, 2], &[1])).await?;
        // Until the client hangs up, so its close is what's logged as "WS closed".
        while socket.next().await.is_some_and(|message| message.is_ok()) {}
        return Ok(());
    }
    send(socket, &pdu([0, 0, 0, 2], &[0])).await?;
    ensure!(receive(socket).await? == pdu([0, 0, 0, 7], &[6, 3, 1, 16, 2, 32, 3, 48]), "not the endpoints");
    let plug_out = pdu([0, 0, 0, 5], &[]);
    send(socket, &pdu([0, 0, 0, 4], &[])).await?;
    let mut answer = receive(socket).await?;
    'served: {
        if answer == plug_out {
            break 'served;
        }
        ensure!(answer == pdu([0, 0, 0, 3], &[255; 4]), "not alive");

        let commands: [(&[u8], u32); 3] = [
            (&[0x12, 0, 0, 0, 36, 0], 36),
            (&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8),
            (&[0x28, 0, 0, 0, 0, 1, 0, 0, 1, 0], 2048),
        ];
        for (tag, (command, length)) in (1u32..).zip(commands) {
            let mut wrapper =
                [&b"USBC"[..], &tag.to_le_bytes(), &length.to_le_bytes(), &[0x80, 0, command.len() as u8]].concat();
            wrapper.extend(command);
            wrapper.resize(31, 0);
            send(socket, &pdu([0x11, 0, 2, 0], &wrapper)).await?;
            answer = receive(socket).await?;
            if answer == plug_out {
                break 'served;
            }
            ensure!(answer.len() >= 29, "not an answer");
            let data = &answer[8..answer.len() - 21];
            let Some(residue) = length.checked_sub(data.len() as u32) else { bail!("more than we asked for") };
            let status = [&b"USBS"[..], &tag.to_le_bytes(), &residue.to_le_bytes(), &[0]].concat();
            ensure!(
                answer[..8] == pdu([0x22, 0, 2, 0], data)[..8]
                    && answer[answer.len() - 21..] == pdu([0x22, 0, 2, 0xFF], &status),
                "not a success"
            );
            let hex: String = data.iter().take(8).map(|byte| format!("{byte:02x}")).collect();
            log(bmc, format!("vm {:02x} {hex}", command[0]));
        }
        answer = receive(socket).await?;
    }
    ensure!(answer == plug_out, "not a plug-out");
    log(bmc, "vm out".into());
    ensure!(!bmc.ikvm.hang_up.load(Ordering::SeqCst), "hanging up");
    send(socket, &pdu([0, 0, 0, 6], &[])).await
}

/// The client's next update request, what it sends meanwhile logged.
async fn request(
    bmc: &Bmc,
    socket: &mut Socket,
    logouts: &mut watch::Receiver<()>,
    session: usize,
) -> anyhow::Result<Vec<u8>> {
    loop {
        let message = tokio::select! {
            message = socket.next() => message,
            _ = logouts.changed() => {
                if bmc.ikvm.live(session) {
                    continue;
                }
                send(socket, &session_message(&[0x39, 0, 0, 0, 1, 0, 0, 0, 3])).await?;
                bail!("logged out");
            }
        };
        let (binary, message) = match message {
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => bail!("closed"),
            Some(Ok(message)) => (message.is_binary(), message.into_data()),
        };
        let shape = (message.first(), message.len());
        let allowed = binary && (matches!(shape, (Some(3), 10) | (Some(4 | 5), 18)) || *message == [7, 7, 0x80]);
        let hex: String = message.iter().map(|byte| format!("{byte:02x}")).collect();
        log(bmc, format!("{} {hex}", if allowed { "ikvm" } else { "forbidden" }));
        ensure!(allowed, "a forbidden message");
        if message[0] == 3 {
            return Ok(message.to_vec());
        }
    }
}

/// A session message (0x39) from its first 9 bytes, the count and a code, naming the X10's user at a made-up address.
fn session_message(head: &[u8]) -> Vec<u8> {
    let mut message = [&head[..9], b"1 ADMIN 192.0.2.1"].concat();
    message.resize(265, 0);
    message
}

async fn send(socket: &mut Socket, message: &[u8]) -> anyhow::Result<()> {
    Ok(socket.send(Message::binary(message.to_vec())).await?)
}

async fn receive(socket: &mut Socket) -> anyhow::Result<Vec<u8>> {
    match socket.next().await {
        Some(Ok(Message::Binary(message))) => Ok(message.to_vec()),
        other => bail!("expected a binary message, got {other:?}"),
    }
}

/// Records a short KVM session of `target`'s into `fixture`'s `kvm.transcript`: our update requests, each sent once
/// the last was answered, and what the BMC says, for 3 seconds. Its session messages' users are made up.
pub async fn record(target: &Target, fixture: &Path) -> anyhow::Result<()> {
    let Ikvm { web, mut receiver, mut sender, mut width, mut height } =
        Ikvm::open(&Connection::new(target, None)?).await?;
    let mut transcript = Transcript::default();
    let recording = async {
        let (end, mut whole) = (Instant::now() + Duration::from_secs(3), true);
        while Instant::now() < end {
            transcript.0.push((Direction::Sent, sender.update(whole, width, height).await?));
            loop {
                let message = match receiver.read(1).await?[0] {
                    0 => {
                        let header = receiver.read(23).await?;
                        let len = u32::from_be_bytes(header[19..23].try_into().unwrap());
                        [&[0][..], &header, &receiver.read(len as usize).await?].concat()
                    }
                    0x39 => session_message(&[&[0x39][..], &receiver.read(264).await?].concat()),
                    kind => bail!("an iKVM message we don't record: {kind:02x}"),
                };
                transcript.0.push((Direction::Received, message.clone()));
                if message[0] == 0 {
                    // A new size wants the whole screen, as our client asks for it.
                    let size = [8, 10].map(|at| u16::from_be_bytes([message[at], message[at + 1]]));
                    whole = message.len() > 24 && size != [width, height] && size[0] <= 1920 && size[1] <= 1280;
                    [width, height] = if whole { size } else { [width, height] };
                    break;
                }
            }
        }
        anyhow::Ok(())
    };
    let recorded = recording.await;
    sender.close().await;
    receiver.closed().await;
    web.logout().await;
    recorded?;
    std::fs::create_dir_all(fixture)?;
    transcript.save(&fixture.join("kvm.transcript"))?;
    eprintln!("   KVM  {} messages", transcript.0.len());
    Ok(())
}
