//! WS-Man on HTTPS: answers from the fixture's `wsman/` where it has the reply (see `reply`), by the spec otherwise.

use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use amt_core::Connection;
use amt_core::wsman::resource_uri;
use instant_xml::ser::Context;
use instant_xml::{Id, Serializer, ToXml};
use tokio::sync::Mutex;

use crate::scrub::scrub;
use crate::{Amt, REALM, digest};

pub async fn serve(listener: TcpListener, tls: TlsAcceptor, amt: Arc<Amt>) {
    while let Ok((tcp, _)) = listener.accept().await {
        amt.connections.fetch_add(1, Ordering::SeqCst);
        let (tls, amt) = (tls.clone(), amt.clone());
        tokio::spawn(async move {
            let Ok(stream) = tls.accept(tcp).await else { return };
            let service = service_fn(move |request| {
                let amt = amt.clone();
                async move { Ok::<_, Infallible>(respond(&amt, request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

async fn respond(amt: &Amt, request: Request<Incoming>) -> Response<Full<Bytes>> {
    assert!(request.headers().contains_key("content-length"), "AMT needs a Content-Length");
    assert!(!request.headers().contains_key("transfer-encoding"), "AMT rejects chunked bodies");

    let authorization = request.headers().get("authorization").and_then(|value| value.to_str().ok());
    if !authorization.is_some_and(|header| authorized(amt, header)) {
        amt.challenges.fetch_add(1, Ordering::SeqCst);
        let nonce = amt.nonce.lock().unwrap().clone();
        let challenge = format!(r#"Digest realm="{REALM}", nonce="{nonce}", stale="false", qop="auth""#);
        let response = Response::builder().status(StatusCode::UNAUTHORIZED).header("www-authenticate", challenge);
        return response.body(Full::default()).unwrap();
    }

    let body = String::from_utf8(request.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let (status, xml) =
        answer(amt, &body).await.unwrap_or_else(|| (StatusCode::BAD_REQUEST, envelope(Fault::unreachable())));
    amt.requests.lock().unwrap().push(body);
    Response::builder().status(status).body(Full::new(xml.into())).unwrap()
}

/// What we answer to a request we can read: what was recorded, or what the spec says.
async fn answer(amt: &Amt, body: &str) -> Option<(StatusCode, String)> {
    let doc = roxmltree::Document::parse(body).ok()?;
    let text = |name| doc.descendants().find(|node| node.has_tag_name(name)).and_then(|node| node.text());
    let class = text("ResourceURI")?.rsplit('/').next().filter(|class| plain(class))?;
    let action = text("Action")?.rsplit('/').next().filter(|action| plain(action))?;
    if let Some(device) = &amt.device {
        return Some(forward(amt, device, class, action, body).await);
    }

    // A device we recorded has the reply under its `reply` name, Intel's samples have one per class and action.
    let fixture = |name: Option<String>| std::fs::read_to_string(amt.fixture.join("wsman").join(name?)).ok();
    let recorded = |action: &str| {
        let sample = amt.samples.get(&(class.to_owned(), action.to_lowercase()));
        fixture(reply(class, action, body)).or_else(|| std::fs::read_to_string(sample?).ok())
    };
    // An enumeration's pages, from 1, as `Class.Pull.xml`, `Class.Pull.2.xml` and so on.
    let page = |n: usize| if n == 1 { recorded("Pull") } else { fixture(Some(format!("{class}.Pull.{n}.xml"))) };
    let ok = |xml| Some((StatusCode::OK, xml));
    match action {
        "Get" => ok(recorded("Get")?),
        // A class we can pull is one we can enumerate, the context only has to lead the first Pull back here.
        "Enumerate" => {
            page(1)?;
            ok(envelope(EnumerateResponse { EnumerationContext: class }))
        }
        // Each page leads to the next by the context it ends with, after the last we say there are no more.
        "Pull" => {
            let context = text("EnumerationContext")?;
            let next = if context == class {
                Some(1)
            } else {
                (1..).map_while(|n| Some((n, page(n)?))).find_map(|(n, xml)| {
                    (amt_core::wsman::parse_pull(&xml).ok()?.next.as_deref() == Some(context)).then_some(n + 1)
                })
            };
            ok(next.and_then(page).unwrap_or_else(|| envelope(PullResponse { EndOfSequence })))
        }
        // Methods we have no reply for (the ones that change something) simply succeed.
        method => ok(recorded(method).unwrap_or_else(|| envelope(Output { method, class }))),
    }
}

/// Class, method and field names as AMT has them: letters, digits and underscores.
fn plain(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|char| char.is_ascii_alphanumeric() || char == '_')
}

/// Recording: the device answers, and we keep what it says to methods (the crawl has the rest).
async fn forward(amt: &Amt, device: &Mutex<Connection>, class: &str, action: &str, body: &str) -> (StatusCode, String) {
    match device.lock().await.post(body.to_owned()).await {
        Ok(xml) => {
            if let Some(file) = reply(class, action, body).filter(|_| !matches!(action, "Get" | "Enumerate" | "Pull")) {
                let dir = amt.fixture.join("wsman");
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(file), scrub(&xml))).unwrap();
            }
            (StatusCode::OK, xml)
        }
        Err(error) => (StatusCode::BAD_REQUEST, envelope(Fault::new(&error.to_string()))),
    }
}

fn authorized(amt: &Amt, header: &str) -> bool {
    let param = |name: &str| {
        let value = header.split(", ").find_map(|param| param.strip_prefix(&format!("{name}=")))?;
        Some(value.trim_matches('"'))
    };
    let (Some(nonce), Some(nc), Some(cnonce), Some(response)) =
        (param("nonce"), param("nc"), param("cnonce"), param("response"))
    else {
        return false;
    };
    nonce == *amt.nonce.lock().unwrap() && response == digest("/wsman", nonce, nc, cnonce, "auth")
}

const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
const ENUMERATION: &str = "http://schemas.xmlsoap.org/ws/2004/09/enumeration";

/// Our answers always serialize: their every part is a type of ours.
fn envelope(body: impl ToXml) -> String {
    let xml = instant_xml::to_string(&Envelope { Header, Body: Body { content: body } });
    String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#) + &xml.expect("our answers serialize")
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Envelope<T> {
    Header: Header,
    Body: Body<T>,
}

#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Header;

#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Body<T> {
    content: T,
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct EnumerateResponse<'a> {
    EnumerationContext: &'a str,
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct PullResponse {
    EndOfSequence: EndOfSequence,
}

#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct EndOfSequence;

/// A method that went well: `<{method}_OUTPUT>` with a zero `ReturnValue`, in its class's namespace. The name comes
/// from the request, which a derived type can't do.
struct Output<'a> {
    method: &'a str,
    class: &'a str,
}

impl ToXml for Output<'_> {
    fn serialize<W: fmt::Write + ?Sized>(
        &self,
        _field: Option<Id<'_>>,
        serializer: &mut Serializer<'_, W>,
    ) -> Result<(), instant_xml::Error> {
        let (name, uri) = (format!("{}_OUTPUT", self.method), resource_uri(self.class));
        let element = serializer.write_start(&name, &uri, None::<Context<0>>)?;
        serializer.end_start()?;
        0u32.serialize(Some(Id { ns: &uri, name: "ReturnValue" }), serializer)?;
        serializer.write_close(element)
    }
}

/// A SOAP fault, of which our client reads the reason.
#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Fault<'a> {
    Reason: Reason<'a>,
}

impl<'a> Fault<'a> {
    fn new(reason: &'a str) -> Self {
        Self { Reason: Reason { Text: reason } }
    }

    /// What AMT says for a class it doesn't have.
    fn unreachable() -> Self {
        Self::new("No route can be determined to reach the destination role defined by the WSAddressing To.")
    }
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Reason<'a> {
    Text: &'a str,
}

/// Where a request's reply lives in a fixture: `<Class>.<Action>.xml`, plus the input for methods that take one, like
/// `AMT_AuditLog.ReadRecords.StartIndex=1.xml`. Paging works out because the next request's input comes from the reply.
/// Inputs that wouldn't make a plain file name have no reply.
pub fn reply(class: &str, action: &str, body: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(body).ok()?;
    let input = doc.descendants().find(|node| node.tag_name().name() == format!("{action}_INPUT"));
    let fields = input.iter().flat_map(|input| input.children().filter(|field| field.is_element()));
    let fields: Vec<_> =
        fields.map(|field| format!("{}={}", field.tag_name().name(), field.text().unwrap_or_default())).collect();
    let name = match fields.join(",") {
        input if input.is_empty() => format!("{class}.{action}.xml"),
        input => format!("{class}.{action}.{input}.xml"),
    };
    name.chars().all(|char| char.is_ascii_alphanumeric() || "_=,.-".contains(char)).then_some(name)
}
