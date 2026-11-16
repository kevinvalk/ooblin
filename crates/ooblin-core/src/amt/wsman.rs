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

/// The resource URI of a class, which is also the namespace of its instances and method inputs.
pub fn resource_uri(class: &str) -> String {
    let schema = match class.split('_').next() {
        Some("AMT") => "http://intel.com/wbem/wscim/1/amt-schema/1/",
        Some("IPS") => "http://intel.com/wbem/wscim/1/ips-schema/1/",
        _ => "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/",
    };
    format!("{schema}{class}")
}

/// The input of a method, which is `{NAME}_INPUT` in the namespace of its class, `URI`.
pub trait Method: ToXml {
    const URI: &'static str;
    const NAME: &'static str;
}

/// Makes `$input` the input of the method in `$uri` that it is named after, or of `$name`.
macro_rules! method {
    ($input:ident$(<$lifetime:lifetime>)?, $uri:expr) => {
        $crate::amt::wsman::method!($input$(<$lifetime>)?, $uri, stringify!($input));
    };
    ($input:ident$(<$lifetime:lifetime>)?, $uri:expr, $name:expr) => {
        impl $crate::amt::wsman::Method for $input$(<$lifetime>)? {
            const URI: &'static str = $uri;
            const NAME: &'static str = $name;
        }
    };
}
pub(crate) use method;

pub fn get(class: &str, message_id: u32) -> String {
    envelope(&format!("{TRANSFER}/Get"), &resource_uri(class), &[], message_id, None::<Enumerate>)
}

pub fn enumerate(class: &str, message_id: u32) -> String {
    envelope(&format!("{ENUMERATION}/Enumerate"), &resource_uri(class), &[], message_id, Some(Enumerate))
}

pub fn pull(class: &str, context: &str, message_id: u32) -> String {
    let pull = Pull { EnumerationContext: context, MaxElements: 999, MaxCharacters: 99_999 };
    envelope(&format!("{ENUMERATION}/Pull"), &resource_uri(class), &[], message_id, Some(pull))
}

/// Writes back an instance of `class` whole. Where the class has several, the `keys` pick the one.
pub fn put(class: &str, keys: &[(&str, Key)], instance: &impl ToXml, message_id: u32) -> String {
    envelope(&format!("{TRANSFER}/Put"), &resource_uri(class), keys, message_id, Some(instance))
}

pub fn delete(class: &str, id: &str, message_id: u32) -> String {
    let keys = [("InstanceID", Key::Value(id))];
    envelope(&format!("{TRANSFER}/Delete"), &resource_uri(class), &keys, message_id, None::<Enumerate>)
}

/// Calls a method of the class, or with an `id` of its instance with that `InstanceID`.
pub fn invoke<T: Method>(input: &T, id: Option<&str>, message_id: u32) -> String {
    let keys: Vec<_> = id.map(|id| ("InstanceID", Key::Value(id))).into_iter().collect();
    envelope(&format!("{}/{}", T::URI, T::NAME), T::URI, &keys, message_id, Some(input))
}

/// What picks one instance of several: a value, or a reference to another instance.
pub enum Key<'a> {
    Value(&'a str),
    Reference(Reference<'a>),
}

impl ToXml for Key<'_> {
    fn serialize<W: fmt::Write + ?Sized>(
        &self,
        _field: Option<Id<'_>>,
        serializer: &mut Serializer<'_, W>,
    ) -> Result<(), instant_xml::Error> {
        match self {
            Key::Value(value) => value.serialize(None, serializer),
            Key::Reference(reference) => {
                reference.serialize(Some(Id { ns: ADDRESSING, name: "EndpointReference" }), serializer)
            }
        }
    }
}

// IMPORTANT: Requests are typed XML (instant-xml): every element is one of our types or fields and every value is
// escaped by the serializer, so nothing a device or a user says can change the shape of a request. We shape them like
// go-wsman-messages does (Copyright Intel Corporation, Apache-2.0), because AMT is known to take those.
fn envelope(action: &str, uri: &str, keys: &[(&str, Key)], message_id: u32, body: Option<impl ToXml>) -> String {
    let selectors: Vec<_> = keys.iter().map(|(name, value)| Selector { Name: name, value }).collect();
    let header = Header {
        Action: action,
        To: "/wsman",
        ResourceURI: uri,
        MessageID: message_id,
        ReplyTo: ReplyTo { Address: "http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous" },
        OperationTimeout: "PT60S",
        SelectorSet: (!selectors.is_empty()).then_some(SelectorSet { Selector: &selectors }),
    };

    let mut xml = String::from(r#"<?xml version="1.0" encoding="utf-8"?>"#);
    instant_xml::to_writer(&Envelope { Header: header, Body: Body { content: body } }, &mut xml)
        .expect("A request that does not serialize, is every part of it still a type of ours?!");

    xml
}

#[derive(ToXml)]
#[xml(ns(SOAP, a = ADDRESSING, w = WSMAN))]
struct Envelope<'a, T> {
    Header: Header<'a>,
    Body: Body<T>,
}

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
    SelectorSet: Option<SelectorSet<'a>>,
}

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

#[derive(ToXml)]
#[xml(ns(ENUMERATION))]
struct Pull<'a> {
    EnumerationContext: &'a str,
    MaxElements: u32,
    MaxCharacters: u32,
}

/// A reference to an instance (a WS-Addressing endpoint reference): its class and the keys that pick it out. We write
/// it as the field that holds it (`<h:ManagedElement>`, say) and a derived type cannot do that, so we serialize it by
/// hand.
#[derive(Clone, Copy)]
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

        let keys: Vec<_> = self.selectors.iter().map(|&(name, value)| (name, Key::Value(value))).collect();
        let selectors: Vec<_> = keys.iter().map(|(name, value)| Selector { Name: name, value }).collect();
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

#[derive(ToXml)]
#[xml(ns(ADDRESSING))]
struct ReferenceParameters<'a> {
    #[xml(ns(WSMAN))]
    ResourceURI: &'a str,
    SelectorSet: SelectorSet<'a>,
}

#[derive(ToXml)]
#[xml(ns(WSMAN))]
struct SelectorSet<'a> {
    Selector: &'a [Selector<'a>],
}

#[derive(ToXml)]
#[xml(ns(WSMAN))]
struct Selector<'a> {
    #[xml(attribute)]
    Name: &'a str,
    #[xml(direct)]
    value: &'a Key<'a>,
}

/// A CIM instance with its properties in document order, where an array property appears once per element. We trim
/// values (AMT pads part numbers) and collapse nested values to their texts, a line each: `<ReleaseDate><Datetime>` to
/// its date, a reference to its address, resource URI and selector values.
#[derive(Debug)]
pub struct Instance {
    pub class: String,
    pub properties: Vec<(String, String)>,
}

impl Instance {
    /// The value of the property, empty when the instance does not have it.
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

    /// Changes a property for a Put, and adds it when the instance does not have it.
    pub fn set(&mut self, name: &str, value: impl ToString) {
        self.set_all(name, [value]);
    }

    /// Changes an array property for a Put. With no `values`, the property is left out.
    pub fn set_all(&mut self, name: &str, values: impl IntoIterator<Item = impl ToString>) {
        // We put the values where the first one was, an index that taking them all out leaves in place.
        let index = self.properties.iter().position(|(key, _)| key == name).unwrap_or(self.properties.len());
        self.properties.retain(|(key, _)| key != name);

        let values = values.into_iter().map(|value| (name.to_owned(), value.to_string()));
        self.properties.splice(index..index, values);
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

// Like MeshCommander, we write an instance back for a Put as we read it: AMT wants it whole, and this way we keep what
// we do not know of it.
// WARNING: This only works for instances of plain values, as we read a nested one (a reference) as just its texts!
impl ToXml for Instance {
    fn serialize<W: fmt::Write + ?Sized>(
        &self,
        _field: Option<Id<'_>>,
        serializer: &mut Serializer<'_, W>,
    ) -> Result<(), instant_xml::Error> {
        let uri = resource_uri(&self.class);
        let element = serializer.write_start(&self.class, &uri, None::<Context<0>>)?;
        serializer.end_start()?;

        for (name, value) in &self.properties {
            value.serialize(Some(Id { ns: &uri, name }), serializer)?;
        }

        serializer.write_close(element)
    }
}

pub struct Page {
    pub instances: Vec<Instance>,
    pub next: Option<String>,
}

// NOTE: We read responses by local name, because the namespace prefixes of AMT differ per reply.

pub fn parse_instance(xml: &str) -> Result<Instance> {
    let document = parse(xml)?;
    let instance = elements(body(&document)?).next().ok_or_else(|| missing("instance"))?;

    Ok(Instance::from_node(instance))
}

pub fn parse_enumerate(xml: &str) -> Result<String> {
    let document = parse(xml)?;
    let context = descendant(body(&document)?, "EnumerationContext").ok_or_else(|| missing("EnumerationContext"))?;

    Ok(context.text().unwrap_or_default().to_owned())
}

pub fn parse_pull(xml: &str) -> Result<Page> {
    let document = parse(xml)?;
    let body = body(&document)?;

    let instances = descendant(body, "Items").map(|items| elements(items).map(Instance::from_node).collect());
    let next = match descendant(body, "EndOfSequence") {
        Some(_) => None,
        None => descendant(body, "EnumerationContext").and_then(|context| context.text()).map(str::to_owned),
    };

    Ok(Page { instances: instances.unwrap_or_default(), next })
}

/// The SOAP fault of an error response as `<subcode>: <reason>`.
pub fn fault(xml: &str) -> Option<String> {
    let document = parse(xml).ok()?;
    let fault = descendant(document.root_element(), "Fault")?;
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

fn body<'a>(document: &'a Document) -> Result<Node<'a, 'a>> {
    elements(document.root_element()).find(|node| node.has_tag_name("Body")).ok_or_else(|| missing("Body"))
}

fn elements<'a>(node: Node<'a, 'a>) -> impl Iterator<Item = Node<'a, 'a>> {
    node.children().filter(Node::is_element)
}

fn descendant<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.descendants().find(|node| node.has_tag_name(name))
}

fn missing(element: &str) -> Error {
    Error::Protocol(format!("WS-Man response without {element}"))
}

/// What a document says, element by element: namespace, name, attributes and text, whatever its prefixes are.
#[cfg(test)]
pub(crate) fn meaning(xml: &str) -> String {
    describe(parse(xml).unwrap().root_element())
}

/// What a request's input or instance says, see `meaning`.
#[cfg(test)]
pub(crate) fn body_meaning(envelope: &str) -> String {
    let document = parse(envelope).unwrap();
    describe(elements(body(&document).unwrap()).next().expect("a body with content"))
}

#[cfg(test)]
fn describe(node: Node) -> String {
    let name = node.tag_name();
    let attributes: String =
        node.attributes().map(|attribute| format!(" {}={:?}", attribute.name(), attribute.value())).collect();
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
        // This is go-wsman-messages' Get template, rendered for message ID 0.
        let expected = r#"<?xml version="1.0" encoding="utf-8"?><Envelope xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns="http://www.w3.org/2003/05/soap-envelope"><Header><a:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/Get</a:Action><a:To>/wsman</a:To><w:ResourceURI>http://intel.com/wbem/wscim/1/amt-schema/1/AMT_GeneralSettings</w:ResourceURI><a:MessageID>0</a:MessageID><a:ReplyTo><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo><w:OperationTimeout>PT60S</w:OperationTimeout></Header><Body></Body></Envelope>"#;
        assert_eq!(meaning(&get("AMT_GeneralSettings", 0)), meaning(expected));
    }

    #[test]
    fn picks_an_instance_like_go_wsman_messages() {
        // This is go-wsman-messages' header for a method of one instance, from `ethernetport/settings_test.go`.
        let expected = r#"<Header xmlns="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><a:Action>http://intel.com/wbem/wscim/1/amt-schema/1/AMT_SystemPowerScheme/SetPowerScheme</a:Action><a:To>/wsman</a:To><w:ResourceURI>http://intel.com/wbem/wscim/1/amt-schema/1/AMT_SystemPowerScheme</w:ResourceURI><a:MessageID>0</a:MessageID><a:ReplyTo><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo><w:OperationTimeout>PT60S</w:OperationTimeout><w:SelectorSet><w:Selector Name="InstanceID">SCHEME 1</w:Selector></w:SelectorSet></Header>"#;
        let request = invoke(&crate::amt::settings::SetPowerScheme, Some("SCHEME 1"), 0);
        let document = parse(&request).unwrap();
        let header = elements(document.root_element()).find(|node| node.has_tag_name("Header")).unwrap();
        assert_eq!(describe(header), meaning(expected));
    }

    #[test]
    fn keeps_what_a_device_says_a_value() {
        let context = "1</EnumerationContext><Injected/>&";
        let request = pull("AMT_GeneralSettings", context, 0);
        let document = Document::parse(&request).unwrap();
        let read = descendant(document.root_element(), "EnumerationContext").and_then(|node| node.text());
        assert_eq!((read, descendant(document.root_element(), "Injected")), (Some(context), None));
    }
}
