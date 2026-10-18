//! WS-Management envelopes and responses, no I/O. The envelopes are the templates from go-wsman-messages
//! (`internal/message/wsman.go`, Copyright Intel Corporation, Apache-2.0) to the byte, AMT is picky and those are known
//! to work. We read responses by local name since AMT's namespace prefixes differ per reply.

use crate::{Error, Result};
use roxmltree::{Document, Node};

const PREFIX: &str = r#"<?xml version="1.0" encoding="utf-8"?><Envelope xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns="http://www.w3.org/2003/05/soap-envelope">"#;
const ANONYMOUS: &str = "http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous";

const GET: &str = "http://schemas.xmlsoap.org/ws/2004/09/transfer/Get";
const ENUMERATE: &str = "http://schemas.xmlsoap.org/ws/2004/09/enumeration/Enumerate";
const PULL: &str = "http://schemas.xmlsoap.org/ws/2004/09/enumeration/Pull";
const PUT: &str = "http://schemas.xmlsoap.org/ws/2004/09/transfer/Put";

fn resource_uri(class: &str) -> String {
    let schema = match class.split('_').next() {
        Some("AMT") => "http://intel.com/wbem/wscim/1/amt-schema/1/",
        Some("IPS") => "http://intel.com/wbem/wscim/1/ips-schema/1/",
        _ => "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/",
    };
    format!("{schema}{class}")
}

pub fn get(class: &str, message_id: u32) -> String {
    envelope(GET, class, message_id, "<Body></Body>")
}

pub fn enumerate(class: &str, message_id: u32) -> String {
    envelope(
        ENUMERATE,
        class,
        message_id,
        r#"<Body><Enumerate xmlns="http://schemas.xmlsoap.org/ws/2004/09/enumeration" /></Body>"#,
    )
}

pub fn pull(class: &str, context: &str, message_id: u32) -> String {
    let body = format!(
        r#"<Body><Pull xmlns="http://schemas.xmlsoap.org/ws/2004/09/enumeration"><EnumerationContext>{context}</EnumerationContext><MaxElements>999</MaxElements><MaxCharacters>99999</MaxCharacters></Pull></Body>"#
    );
    envelope(PULL, class, message_id, &body)
}

/// Replaces the instance with `instance`, its `<h:{class}>` element.
pub fn put(class: &str, instance: &str, message_id: u32) -> String {
    envelope(PUT, class, message_id, &format!("<Body>{instance}</Body>"))
}

/// A method's `<h:{method}_INPUT>` element the way go-wsman-messages writes it.
pub fn input(class: &str, method: &str, fields: &[(&str, &str)]) -> String {
    let fields: String = fields.iter().map(|(name, value)| format!("<h:{name}>{value}</h:{name}>")).collect();
    format!(r#"<h:{method}_INPUT xmlns:h="{}">{fields}</h:{method}_INPUT>"#, resource_uri(class))
}

/// A method call, `input` being the `<h:{method}_INPUT>` element (see `input`).
pub fn invoke(class: &str, method: &str, input: &str, message_id: u32) -> String {
    let action = format!("{}/{method}", resource_uri(class));
    envelope(&action, class, message_id, &format!("<Body>{input}</Body>"))
}

fn envelope(action: &str, class: &str, message_id: u32, body: &str) -> String {
    let uri = resource_uri(class);
    format!(
        "{PREFIX}<Header><a:Action>{action}</a:Action><a:To>/wsman</a:To><w:ResourceURI>{uri}</w:ResourceURI><a:MessageID>{message_id}</a:MessageID><a:ReplyTo><a:Address>{ANONYMOUS}</a:Address></a:ReplyTo><w:OperationTimeout>PT60S</w:OperationTimeout></Header>{body}</Envelope>"
    )
}

/// A CIM instance with its properties in document order, array properties appear once per element. Values are trimmed
/// (AMT pads part numbers) and nested values collapse to their texts, a line each: `<ReleaseDate><Datetime>` to its
/// date, a reference to its address, resource URI and selector values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub class: String,
    pub properties: Vec<(String, String)>,
}

impl Instance {
    /// The property's value, empty when the instance doesn't have it.
    pub fn text(&self, name: &str) -> String {
        self.get(name).unwrap_or_default().to_owned()
    }

    /// Every value of an array property, in document order.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.properties.iter().filter(move |(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.properties.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn parse<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.get(name)?.parse().ok()
    }

    fn from_node(node: Node) -> Self {
        Self {
            class: node.tag_name().name().to_owned(),
            properties: elements(node)
                .map(|property| {
                    let texts = property.descendants().filter(Node::is_text).map(|text| text.text().unwrap().trim());
                    let value = texts.filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n");
                    (property.tag_name().name().to_owned(), value)
                })
                .collect(),
        }
    }
}

pub struct Page {
    pub items: Vec<Instance>,
    pub next: Option<String>,
}

pub fn parse_instance(xml: &str) -> Result<Instance> {
    let doc = parse(xml)?;
    let instance = elements(body(&doc)?).next().ok_or_else(|| missing("instance"))?;
    Ok(Instance::from_node(instance))
}

pub fn parse_enumerate(xml: &str) -> Result<String> {
    let doc = parse(xml)?;
    let context = descendant(body(&doc)?, "EnumerationContext").ok_or_else(|| missing("EnumerationContext"))?;
    Ok(context.text().unwrap_or_default().to_owned())
}

pub fn parse_pull(xml: &str) -> Result<Page> {
    let doc = parse(xml)?;
    let body = body(&doc)?;
    let items = descendant(body, "Items").map(|items| elements(items).map(Instance::from_node).collect());
    let next = match descendant(body, "EndOfSequence") {
        Some(_) => None,
        None => descendant(body, "EnumerationContext").and_then(|context| context.text()).map(str::to_owned),
    };
    Ok(Page { items: items.unwrap_or_default(), next })
}

/// The SOAP fault of an error response as `<subcode>: <reason>`.
pub fn fault(xml: &str) -> Option<String> {
    let doc = parse(xml).ok()?;
    let fault = descendant(doc.root_element(), "Fault")?;
    let text = |name| descendant(fault, name).and_then(|node| node.text()).map(str::trim);
    let code = fault.descendants().rfind(|node| node.has_tag_name("Value")).and_then(|node| node.text());
    Some(match (code, text("Text")) {
        (Some(code), Some(reason)) => format!("{code}: {reason}"),
        (code, reason) => code.or(reason).unwrap_or("unspecified").to_owned(),
    })
}

fn parse(xml: &str) -> Result<Document<'_>> {
    Document::parse(xml.trim_start()).map_err(|error| Error::Protocol(format!("invalid XML: {error}")))
}

fn body<'a>(doc: &'a Document) -> Result<Node<'a, 'a>> {
    elements(doc.root_element()).find(|node| node.has_tag_name("Body")).ok_or_else(|| missing("Body"))
}

fn elements<'a>(node: Node<'a, 'a>) -> impl Iterator<Item = Node<'a, 'a>> {
    node.children().filter(Node::is_element)
}

fn descendant<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.descendants().find(|node| node.has_tag_name(name))
}

fn missing(what: &str) -> Error {
    Error::Protocol(format!("WS-Man response without {what}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_envelopes_like_go_wsman_messages() {
        // go-wsman-messages' Get template, rendered for message ID 0.
        assert_eq!(
            super::get("AMT_GeneralSettings", 0),
            r#"<?xml version="1.0" encoding="utf-8"?><Envelope xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns="http://www.w3.org/2003/05/soap-envelope"><Header><a:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/Get</a:Action><a:To>/wsman</a:To><w:ResourceURI>http://intel.com/wbem/wscim/1/amt-schema/1/AMT_GeneralSettings</w:ResourceURI><a:MessageID>0</a:MessageID><a:ReplyTo><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo><w:OperationTimeout>PT60S</w:OperationTimeout></Header><Body></Body></Envelope>"#
        );
    }
}
