//! WS-Man on HTTPS: answers from the fixture's `wsman/` where it has the reply (see `reply`), by the spec otherwise.

use std::convert::Infallible;
use std::path::Path;
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
use tokio::sync::Mutex;

use crate::scrub::scrub;
use crate::{Amt, REALM, digest};

/// What AMT answers for a class it doesn't have.
const UNREACHABLE: &str = r#"<a:Fault xmlns:a="http://www.w3.org/2003/05/soap-envelope"><a:Code><a:Value>a:Sender</a:Value><a:Subcode><a:Value>b:DestinationUnreachable</a:Value></a:Subcode></a:Code><a:Reason><a:Text xml:lang="en-US">No route can be determined to reach the destination role defined by the WSAddressing To.</a:Text></a:Reason></a:Fault>"#;

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
    let between = |open: &str| {
        let start = body.find(open).unwrap() + open.len();
        body[start..start + body[start..].find('<').unwrap()].to_owned()
    };
    let uri = between("<w:ResourceURI>");
    if let Some(device) = &amt.device {
        let action = between("<a:Action>").rsplit('/').next().unwrap().to_owned();
        return forward(amt, device, uri.rsplit('/').next().unwrap(), &action, body).await;
    }
    let (class, action) =
        (uri.rsplit('/').next().unwrap(), between("<a:Action>").rsplit('/').next().unwrap().to_owned());
    // A device we recorded has the reply under its `reply` name, Intel's samples have one per class and action.
    let recorded = |action: &str| {
        let sample = amt.samples.get(&(class.to_owned(), action.to_lowercase()));
        let read = |path: &Path| std::fs::read_to_string(path).ok();
        read(&amt.fixture.join("wsman").join(reply(class, action, &body)))
            .or_else(|| sample.and_then(|path| read(path)))
    };

    let (status, xml) = match action.as_str() {
        "Get" => recorded("Get").map_or((StatusCode::BAD_REQUEST, envelope(UNREACHABLE)), |xml| (StatusCode::OK, xml)),
        "Pull" => recorded("Pull")
            .map_or((StatusCode::BAD_REQUEST, envelope(UNREACHABLE)), |xml| (StatusCode::OK, last_page(xml))),
        // A class we can pull is one we can enumerate, the context only has to lead the Pull back here.
        "Enumerate" if recorded("Pull").is_some() => (
            StatusCode::OK,
            envelope(&format!(
                r#"<g:EnumerateResponse xmlns:g="http://schemas.xmlsoap.org/ws/2004/09/enumeration"><g:EnumerationContext>{class}</g:EnumerationContext></g:EnumerateResponse>"#
            )),
        ),
        "Enumerate" => (StatusCode::BAD_REQUEST, envelope(UNREACHABLE)),
        // Methods we have no reply for (the ones that change something) simply succeed.
        method => recorded(method).map_or_else(
            || {
                (
                    StatusCode::OK,
                    envelope(&format!(
                        r#"<h:{method}_OUTPUT xmlns:h="{uri}"><h:ReturnValue>0</h:ReturnValue></h:{method}_OUTPUT>"#
                    )),
                )
            },
            |xml| (StatusCode::OK, xml),
        ),
    };
    amt.requests.lock().unwrap().push(body);
    Response::builder().status(status).body(Full::new(xml.into())).unwrap()
}

/// Recording: the device answers, and we keep what it says to methods (the crawl has the rest).
async fn forward(
    amt: &Amt,
    device: &Mutex<Connection>,
    class: &str,
    action: &str,
    body: String,
) -> Response<Full<Bytes>> {
    let file = reply(class, action, &body);
    match device.lock().await.post(body).await {
        Ok(xml) => {
            if !matches!(action, "Get" | "Enumerate" | "Pull") {
                let dir = amt.fixture.join("wsman");
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(file), scrub(&xml))).unwrap();
            }
            Response::new(Full::new(xml.into()))
        }
        Err(error) => {
            let fault = format!(
                r#"<a:Fault xmlns:a="http://www.w3.org/2003/05/soap-envelope"><a:Reason><a:Text>{error}</a:Text></a:Reason></a:Fault>"#
            );
            Response::builder().status(StatusCode::BAD_REQUEST).body(Full::new(envelope(&fault).into())).unwrap()
        }
    }
}

/// A replayed Pull is all there is. Intel's samples don't always say so, which would have us pull forever.
fn last_page(xml: String) -> String {
    if xml.contains("EndOfSequence") {
        return xml;
    }
    let end = xml.rfind("Items>").map_or(xml.len(), |items| items + "Items>".len());
    let mark = r#"<g:EndOfSequence xmlns:g="http://schemas.xmlsoap.org/ws/2004/09/enumeration"/>"#;
    format!("{}{mark}{}", &xml[..end], &xml[end..])
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

fn envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><a:Envelope xmlns:a="http://www.w3.org/2003/05/soap-envelope"><a:Header></a:Header><a:Body>{body}</a:Body></a:Envelope>"#
    )
}

/// Where a request's reply lives in a fixture: `<Class>.<Action>.xml`, plus the input for methods that take one, like
/// `AMT_AuditLog.ReadRecords.StartIndex=1.xml`. Paging works out because the next request's input comes from the reply.
pub fn reply(class: &str, action: &str, body: &str) -> String {
    let input = roxmltree::Document::parse(body).ok().and_then(|doc| {
        let input = doc.descendants().find(|node| node.tag_name().name() == format!("{action}_INPUT"))?;
        let fields = input.children().filter(|field| field.is_element());
        let fields: Vec<_> =
            fields.map(|field| format!("{}={}", field.tag_name().name(), field.text().unwrap_or_default())).collect();
        Some(fields.join(","))
    });
    match input.filter(|input| !input.is_empty()) {
        Some(input) => format!("{class}.{action}.{input}.xml"),
        None => format!("{class}.{action}.xml"),
    }
}
