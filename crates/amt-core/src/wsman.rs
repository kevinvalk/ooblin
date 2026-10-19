//! WS-Management, no I/O. Requests are typed XML (instant-xml): every element is one of our types or fields, every value
//! is escaped by the serializer, so nothing a device or a user says can change a request's shape. They're shaped like
//! go-wsman-messages' (Copyright Intel Corporation, Apache-2.0), which AMT is known to take. We read responses by local
//! name since AMT's namespace prefixes differ per reply.

use std::fmt;

use instant_xml::ser::Context;
use instant_xml::{Id, Serializer, ToXml};
use roxmltree::{Document, Node};

use crate::{Error, Result};

const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
const ADDRESSING: &str = "http://schemas.xmlsoap.org/ws/2004/08/addressing";
const WSMAN: &str = "http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd";
const ENUMERATION: &str = "http://schemas.xmlsoap.org/ws/2004/09/enumeration";
const TRANSFER: &str = "http://schemas.xmlsoap.org/ws/2004/09/transfer";

/// A class's resource URI, which is also the namespace of its instances and method inputs.
pub fn resource_uri(class: &str) -> String {
    let schema = match class.split('_').next() {
        Some("AMT") => "http://intel.com/wbem/wscim/1/amt-schema/1/",
        Some("IPS") => "http://intel.com/wbem/wscim/1/ips-schema/1/",
        _ => "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/",
    };
    format!("{schema}{class}")
}

/// A method's input: `{NAME}_INPUT` in its class's namespace, `URI`.
pub trait Method: ToXml {
    const URI: &'static str;
    const NAME: &'static str;
}

/// An instance we write back whole: in its class's namespace, `URI`.
pub trait Resource: ToXml {
    const URI: &'static str;
}

pub fn get(class: &str, message_id: u32) -> String {
    envelope(&format!("{TRANSFER}/Get"), &resource_uri(class), message_id, None::<Enumerate>)
}

pub fn enumerate(class: &str, message_id: u32) -> String {
    envelope(&format!("{ENUMERATION}/Enumerate"), &resource_uri(class), message_id, Some(Enumerate))
}

pub fn pull(class: &str, context: &str, message_id: u32) -> String {
    let pull = Pull { EnumerationContext: context, MaxElements: 999, MaxCharacters: 99_999 };
    envelope(&format!("{ENUMERATION}/Pull"), &resource_uri(class), message_id, Some(pull))
}

pub fn put<T: Resource>(instance: &T, message_id: u32) -> String {
    envelope(&format!("{TRANSFER}/Put"), T::URI, message_id, Some(instance))
}

pub fn invoke<T: Method>(input: &T, message_id: u32) -> String {
    envelope(&format!("{}/{}", T::URI, T::NAME), T::URI, message_id, Some(input))
}

/// Our requests always serialize: their every part is a type of ours.
fn envelope(action: &str, uri: &str, message_id: u32, body: Option<impl ToXml>) -> String {
    let header = Header {
        Action: action,
        To: "/wsman",
        ResourceURI: uri,
        MessageID: message_id,
        ReplyTo: ReplyTo { Address: "http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous" },
        OperationTimeout: "PT60S",
    };
    let xml = instant_xml::to_string(&Envelope { Header: header, Body: Body { content: body } });
    String::from(r#"<?xml version="1.0" encoding="utf-8"?>"#) + &xml.expect("our requests serialize")
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(SOAP, a = ADDRESSING, w = WSMAN))]
struct Envelope<'a, T> {
    Header: Header<'a>,
    Body: Body<T>,
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Header<'a> {
    #[xml(ns(ADDRESSING))]
    Action: &'a str,
    #[xml(ns(ADDRESSING))]
    To: &'a str,
    #[xml(ns(WSMAN))]
    ResourceURI: &'a str,
    #[xml(ns(ADDRESSING))]
    MessageID: u32,
    ReplyTo: ReplyTo<'a>,
    #[xml(ns(WSMAN))]
    OperationTimeout: &'a str,
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(ADDRESSING))]
struct ReplyTo<'a> {
    Address: &'a str,
}

#[derive(ToXml)]
#[xml(ns(SOAP))]
struct Body<T> {
    content: Option<T>,
}

#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct Enumerate;

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct Pull<'a> {
    EnumerationContext: &'a str,
    MaxElements: u32,
    MaxCharacters: u32,
}

/// A reference to an instance (a WS-Addressing endpoint reference): its class and the keys that pick it out. It's
/// written as the field that holds it, `<h:ManagedElement>` say, which a derived type can't do.
pub struct Reference<'a> {
    pub class: &'a str,
    pub selectors: &'a [(&'a str, &'a str)],
}

impl ToXml for Reference<'_> {
    fn serialize<W: fmt::Write + ?Sized>(
        &self,
        field: Option<Id<'_>>,
        serializer: &mut Serializer<'_, W>,
    ) -> Result<(), instant_xml::Error> {
        let field = field.ok_or(instant_xml::Error::UnexpectedState("a reference is always a field"))?;
        let element = serializer.write_start(field.name, field.ns, None::<Context<0>>)?;
        serializer.end_start()?;
        let selectors: Vec<_> = self.selectors.iter().map(|&(name, value)| Selector { Name: name, value }).collect();
        let parameters = ReferenceParameters {
            ResourceURI: &resource_uri(self.class),
            SelectorSet: SelectorSet { Selector: &selectors },
        };
        Address { value: ADDRESSING }.serialize(None, serializer)?;
        parameters.serialize(None, serializer)?;
        serializer.write_close(element)
    }
}

#[derive(ToXml)]
#[xml(ns(ADDRESSING))]
struct Address<'a> {
    #[xml(direct)]
    value: &'a str,
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(ADDRESSING))]
struct ReferenceParameters<'a> {
    #[xml(ns(WSMAN))]
    ResourceURI: &'a str,
    SelectorSet: SelectorSet<'a>,
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(WSMAN))]
struct SelectorSet<'a> {
    Selector: &'a [Selector<'a>],
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(ns(WSMAN))]
struct Selector<'a> {
    #[xml(attribute)]
    Name: &'a str,
    #[xml(direct)]
    value: &'a str,
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

/// What a document says, element by element: namespace, name, attributes and text, whatever its prefixes.
#[cfg(test)]
pub(crate) fn meaning(xml: &str) -> String {
    describe(parse(xml).unwrap().root_element())
}

/// What a request's input or instance says, see `meaning`.
#[cfg(test)]
pub(crate) fn body_meaning(envelope: &str) -> String {
    let doc = parse(envelope).unwrap();
    describe(elements(body(&doc).unwrap()).next().expect("a body with content"))
}

#[cfg(test)]
fn describe(node: Node) -> String {
    let name = node.tag_name();
    let attributes: String = node.attributes().map(|attr| format!(" {}={:?}", attr.name(), attr.value())).collect();
    let children: String = node
        .children()
        .map(|child| if child.is_element() { describe(child) } else { child.text().unwrap_or_default().to_owned() })
        .collect();
    format!("<{{{}}}{}{attributes}>{children}</>", name.namespace().unwrap_or_default(), name.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_envelopes_like_go_wsman_messages() {
        // go-wsman-messages' Get template, rendered for message ID 0.
        let expected = r#"<?xml version="1.0" encoding="utf-8"?><Envelope xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns="http://www.w3.org/2003/05/soap-envelope"><Header><a:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/Get</a:Action><a:To>/wsman</a:To><w:ResourceURI>http://intel.com/wbem/wscim/1/amt-schema/1/AMT_GeneralSettings</w:ResourceURI><a:MessageID>0</a:MessageID><a:ReplyTo><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo><w:OperationTimeout>PT60S</w:OperationTimeout></Header><Body></Body></Envelope>"#;
        assert_eq!(meaning(&get("AMT_GeneralSettings", 0)), meaning(expected));
    }

    #[test]
    fn keeps_what_a_device_says_a_value() {
        let context = "1</EnumerationContext><Injected/>&";
        let request = pull("AMT_GeneralSettings", context, 0);
        let doc = Document::parse(&request).unwrap();
        let read = descendant(doc.root_element(), "EnumerationContext").and_then(|node| node.text());
        assert_eq!((read, descendant(doc.root_element(), "Injected")), (Some(context), None));
    }
}
