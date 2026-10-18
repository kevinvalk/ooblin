//! WS-Man on HTTPS: answers from the fixture's `wsman/<Class>.<Action>.xml` where it has them, by the spec otherwise.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

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
    let (class, action) =
        (uri.rsplit('/').next().unwrap(), between("<a:Action>").rsplit('/').next().unwrap().to_owned());
    let recorded = |action: &str| std::fs::read_to_string(amt.fixture.join(format!("wsman/{class}.{action}.xml"))).ok();

    let (status, xml) = match action.as_str() {
        "Get" | "Pull" => {
            recorded(&action).map_or((StatusCode::BAD_REQUEST, envelope(UNREACHABLE)), |xml| (StatusCode::OK, xml))
        }
        // A class we can pull is one we can enumerate, the context only has to lead the Pull back here.
        "Enumerate" if recorded("Pull").is_some() => (
            StatusCode::OK,
            envelope(&format!(
                r#"<g:EnumerateResponse xmlns:g="http://schemas.xmlsoap.org/ws/2004/09/enumeration"><g:EnumerationContext>{class}</g:EnumerationContext></g:EnumerateResponse>"#
            )),
        ),
        "Enumerate" => (StatusCode::BAD_REQUEST, envelope(UNREACHABLE)),
        method => (
            StatusCode::OK,
            envelope(&format!(
                r#"<h:{method}_OUTPUT xmlns:h="{uri}"><h:ReturnValue>0</h:ReturnValue></h:{method}_OUTPUT>"#
            )),
        ),
    };
    amt.requests.lock().unwrap().push(body);
    Response::builder().status(status).body(Full::new(xml.into())).unwrap()
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
