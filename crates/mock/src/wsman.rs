use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::{fmt, io};

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use instant_xml::ser::Context;
use instant_xml::{Id, Serializer, ToXml};
use ooblin_core::amt::Connection;
use ooblin_core::amt::wsman::resource_uri;
use tokio::sync::Mutex;

use crate::scrub::scrub;
use crate::{Amt, REALM, digest};

pub async fn serve(listener: TcpListener, tls: TlsAcceptor, amt: Arc<Amt>) {
    while let Ok((tcp, _)) = listener.accept().await {
        let (tls, amt) = (tls.clone(), amt.clone());
        tokio::spawn(async move {
            let Ok(stream) = tls.accept(tcp).await else { return };
            let service = service_fn(move |request| {
                let amt = amt.clone();
                async move { respond(&amt, request).await }
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

/// Answers a request, or hangs up on it with an error.
async fn respond(amt: &Amt, request: Request<Incoming>) -> io::Result<Response<Full<Bytes>>> {
    // AMT needs a Content-Length, which a chunked body goes without.
    if !request.headers().contains_key("content-length") {
        return Ok(Response::builder().status(StatusCode::LENGTH_REQUIRED).body(Full::default()).unwrap());
    }

    let authorization = request.headers().get("authorization").and_then(|value| value.to_str().ok());
    if !authorization.is_some_and(|header| is_authorized(amt, header)) {
        amt.challenges.fetch_add(1, Ordering::SeqCst);
        let nonce = amt.nonce.lock().unwrap().clone();
        let challenge = format!(r#"Digest realm="{REALM}", nonce="{nonce}", stale="false", qop="auth""#);
        let response = Response::builder().status(StatusCode::UNAUTHORIZED).header("www-authenticate", challenge);
        return Ok(response.body(Full::default()).unwrap());
    }

    let body = request.into_body().collect().await.map_err(io::Error::other)?.to_bytes();
    let body = String::from_utf8_lossy(&body).into_owned();
    let response = match amt.is_hanging_up.swap(false, Ordering::SeqCst) {
        true => Err(io::ErrorKind::ConnectionAborted.into()),
        false => {
            let (status, xml) =
                answer(amt, &body).await.unwrap_or_else(|| (StatusCode::BAD_REQUEST, envelope(Fault::unreachable())));
            Ok(Response::builder().status(status).body(Full::new(xml.into())).unwrap())
        }
    };
    amt.requests.lock().unwrap().push(body);

    response
}

/// What we answer to a request we can read: what was recorded in the `wsman/` of the fixture where it has the reply
/// (see `reply`), or what the specification says.
async fn answer(amt: &Amt, body: &str) -> Option<(StatusCode, String)> {
    let document = roxmltree::Document::parse(body).ok()?;
    let text = |name| document.descendants().find(|node| node.has_tag_name(name)).and_then(|node| node.text());
    let uri = text("ResourceURI")?;
    let class = uri.rsplit('/').next().filter(|class| is_plain(class) && uri == resource_uri(class))?;
    let (namespace, action) = text("Action")?.rsplit_once('/').filter(|(_, action)| is_plain(action))?;

    if let Some(device) = &amt.device {
        return Some(forward(amt, &device.connection, class, action, body).await);
    }

    // A device we recorded has the reply under its `reply` name, the samples of Intel have one per class and action.
    let fixture = |name: Option<String>| std::fs::read_to_string(amt.fixture.join("wsman").join(name?)).ok();
    let recorded = |action: &str| {
        let sample = amt.samples.get(&(class.to_owned(), action.to_lowercase()));
        fixture(reply(class, action, body)).or_else(|| std::fs::read_to_string(sample?).ok())
    };

    // The pages of an enumeration count from 1, as `Class.Pull.xml`, `Class.Pull.2.xml` and so on.
    let page = |number: usize| match number {
        1 => recorded("Pull"),
        _ => fixture(Some(format!("{class}.Pull.{number}.xml"))),
    };
    let ok = |xml| Some((StatusCode::OK, xml));

    match action {
        "Get" => ok(recorded("Get")?),
        // A class we can pull is one we can enumerate, the context only has to lead the first Pull back here.
        "Enumerate" => {
            page(1)?;
            ok(envelope(EnumerateResponse { EnumerationContext: class }))
        }
        // Each page leads to the next by the context it ends with, and after the last one we say there are no more.
        // AMT keeps one context for all pages, so we remember which page each context is at.
        "Pull" => {
            let context = text("EnumerationContext")?;
            let number = if context == class { Some(1) } else { amt.pages.lock().unwrap().remove(context) };
            let Some(xml) = number.and_then(page) else { return ok(envelope(PullResponse { EndOfSequence })) };

            if let Some(next) = ooblin_core::amt::wsman::parse_pull(&xml).ok()?.next {
                amt.pages.lock().unwrap().insert(next, number? + 1);
            }
            ok(xml)
        }
        // A Put carries the instance, a Delete nothing, and a method its input in the namespace of its class, which
        // its action is in too. What we have no reply for (the ones that change something) simply succeeds, on a class
        // we have.
        action => {
            let input = document.descendants().find(|node| node.has_tag_name("Body"))?.first_element_child();
            let input = input.map(|input| input.tag_name());
            let has_fitting_input = match action {
                "Delete" => input.is_none(),
                "Put" => input == Some((uri, class).into()),
                method => namespace == uri && input == Some((uri, &*format!("{method}_INPUT")).into()),
            };

            (has_fitting_input && has_class(amt, class))
                .then(|| recorded(action).unwrap_or_else(|| envelope(Output { method: action, class })))
                .and_then(ok)
        }
    }
}

/// Does the fixture hold anything of `class`, so that the device has it?
fn has_class(amt: &Amt, class: &str) -> bool {
    let mut recorded = std::fs::read_dir(amt.fixture.join("wsman")).into_iter().flatten().flatten();

    amt.samples.keys().any(|(sample, _)| sample == class)
        || recorded.any(|file| file.file_name().to_string_lossy().starts_with(&format!("{class}.")))
}

/// AMT has letters, digits and underscores in the names of its classes, methods and fields, and nothing else.
fn is_plain(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// When recording, the device answers and we keep what it says to methods (the crawl has the rest).
async fn forward(amt: &Amt, device: &Mutex<Connection>, class: &str, action: &str, body: &str) -> (StatusCode, String) {
    match device.lock().await.post(body.to_owned()).await {
        Ok(xml) => {
            if let Some(file) = reply(class, action, body).filter(|_| !matches!(action, "Get" | "Enumerate" | "Pull")) {
                let directory = amt.fixture.join("wsman");
                std::fs::create_dir_all(&directory)
                    .and_then(|()| std::fs::write(directory.join(file), scrub(&xml)))
                    .unwrap();
            }
            (StatusCode::OK, xml)
        }
        Err(error) => (StatusCode::BAD_REQUEST, envelope(Fault::new(&error.to_string()))),
    }
}

fn is_authorized(amt: &Amt, header: &str) -> bool {
    let parameter = |name: &str| {
        let value = header.split(", ").find_map(|parameter| parameter.strip_prefix(&format!("{name}=")))?;
        Some(value.trim_matches('"'))
    };
    let (Some(nonce), Some(nc), Some(cnonce), Some(response)) =
        (parameter("nonce"), parameter("nc"), parameter("cnonce"), parameter("response"))
    else {
        return false;
    };

    parameter("uri") == Some("/wsman")
        && nonce == *amt.nonce.lock().unwrap()
        && response == digest("/wsman", nonce, nc, cnonce, "auth")
}

const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
const ENUMERATION: &str = "http://schemas.xmlsoap.org/ws/2004/09/enumeration";

fn envelope(body: impl ToXml) -> String {
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    instant_xml::to_writer(&Envelope { Header, Body: Body { content: body } }, &mut xml)
        .expect("Every part of an answer of ours is a type of ours, so how can it not serialize?!");

    xml
}

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

#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct EnumerateResponse<'a> {
    EnumerationContext: &'a str,
}

#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct PullResponse {
    EndOfSequence: EndOfSequence,
}

#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct EndOfSequence;

/// A method that went well: `<{method}_OUTPUT>` with a zero `ReturnValue`, in the namespace of its class. The name
/// comes from the request, which a derived type cannot do.
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
#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Fault<'a> {
    Reason: Reason<'a>,
}

impl<'a> Fault<'a> {
    fn new(reason: &'a str) -> Self {
        Self { Reason: Reason { Text: reason } }
    }

    /// What AMT says for a class it does not have, and what we say for anything else we cannot answer.
    fn unreachable() -> Self {
        Self::new("No route can be determined to reach the destination role defined by the WSAddressing To.")
    }
}

#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Reason<'a> {
    Text: &'a str,
}

/// Where the reply to a request lives in a fixture: `<Class>.<Action>.xml`, plus the input for methods that take one,
/// like `AMT_AuditLog.ReadRecords.StartIndex=1.xml`. Paging works out because the input of the next request comes from
/// the reply. An input that would not make a plain file name has no reply.
pub fn reply(class: &str, action: &str, body: &str) -> Option<String> {
    let document = roxmltree::Document::parse(body).ok()?;
    let input = document.descendants().find(|node| node.tag_name().name() == format!("{action}_INPUT"));
    let fields = input.iter().flat_map(|input| input.children().filter(|field| field.is_element()));
    let fields: Vec<_> =
        fields.map(|field| format!("{}={}", field.tag_name().name(), field.text().unwrap_or_default())).collect();
    let name = match fields.join(",") {
        input if input.is_empty() => format!("{class}.{action}.xml"),
        input => format!("{class}.{action}.{input}.xml"),
    };

    name.chars().all(|character| character.is_ascii_alphanumeric() || "_=,.-".contains(character)).then_some(name)
}
