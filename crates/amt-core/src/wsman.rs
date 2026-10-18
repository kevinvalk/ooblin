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

/// A method call, `input` being the `<h:{method}_INPUT>` element from go-wsman-messages.
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
/// (AMT pads part numbers) and nested values like `<ReleaseDate><Datetime>` collapse to their text.
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

    pub fn get(&self, name: &str) -> Option<&str> {
        self.properties.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    fn from_node(node: Node) -> Self {
        Self {
            class: node.tag_name().name().to_owned(),
            properties: elements(node)
                .map(|property| {
                    let mut texts = property.descendants().filter_map(|node| node.text()).map(str::trim);
                    let value = texts.find(|text| !text.is_empty()).unwrap_or_default();
                    (property.tag_name().name().to_owned(), value.to_owned())
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
    let doc = Document::parse(xml).ok()?;
    let fault = descendant(doc.root_element(), "Fault")?;
    let text = |name| descendant(fault, name).and_then(|node| node.text()).map(str::trim);
    let code = fault.descendants().rfind(|node| node.has_tag_name("Value")).and_then(|node| node.text());
    Some(match (code, text("Text")) {
        (Some(code), Some(reason)) => format!("{code}: {reason}"),
        (code, reason) => code.or(reason).unwrap_or("unspecified").to_owned(),
    })
}

fn parse(xml: &str) -> Result<Document<'_>> {
    Document::parse(xml).map_err(|error| Error::Protocol(format!("invalid XML: {error}")))
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
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/go-wsman-messages/wsman");

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
    }

    #[test]
    fn get_matches_go_wsman_messages() {
        // go-wsman-messages' Get template, rendered for message ID 0.
        assert_eq!(
            get("AMT_GeneralSettings", 0),
            r#"<?xml version="1.0" encoding="utf-8"?><Envelope xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:w="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns="http://www.w3.org/2003/05/soap-envelope"><Header><a:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/Get</a:Action><a:To>/wsman</a:To><w:ResourceURI>http://intel.com/wbem/wscim/1/amt-schema/1/AMT_GeneralSettings</w:ResourceURI><a:MessageID>0</a:MessageID><a:ReplyTo><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo><w:OperationTimeout>PT60S</w:OperationTimeout></Header><Body></Body></Envelope>"#
        );
    }

    #[test]
    fn resource_uris_follow_the_class_prefix() {
        assert!(resource_uri("AMT_GeneralSettings").starts_with("http://intel.com/wbem/wscim/1/amt-schema/1/"));
        assert!(resource_uri("IPS_OptInService").starts_with("http://intel.com/wbem/wscim/1/ips-schema/1/"));
        assert!(resource_uri("CIM_Chassis").starts_with("http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/"));
    }

    #[test]
    fn parses_a_get_response() {
        let settings = parse_instance(&fixture("AMT_GeneralSettings.Get.xml")).unwrap();
        assert_eq!(settings.class, "AMT_GeneralSettings");
        assert_eq!(settings.get("HostName"), Some("Test Host Name"));
        assert_eq!(settings.get("DigestRealm"), Some("Digest:F3EB554784E729164447A89F60B641C5"));
    }

    #[test]
    fn parses_an_enumeration() {
        let enumerated = r#"<a:Envelope xmlns:a="http://www.w3.org/2003/05/soap-envelope" xmlns:g="http://schemas.xmlsoap.org/ws/2004/09/enumeration"><a:Body><g:EnumerateResponse><g:EnumerationContext>E2020000-0000-0000-0000-000000000000</g:EnumerationContext></g:EnumerateResponse></a:Body></a:Envelope>"#;
        assert_eq!(parse_enumerate(enumerated).unwrap(), "E2020000-0000-0000-0000-000000000000");
        let page = parse_pull(&fixture("CIM_SoftwareIdentity.Pull.xml")).unwrap();
        assert!(page.next.is_none());
        let amt = page.items.iter().find(|item| item.get("InstanceID") == Some("AMT")).unwrap();
        assert_eq!(amt.get("VersionString"), Some("12.0.67"));
    }

    #[test]
    fn reads_soap_faults() {
        let xml = r#"<a:Envelope xmlns:a="http://www.w3.org/2003/05/soap-envelope" xmlns:c="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><a:Body><a:Fault><a:Code><a:Value>a:Sender</a:Value><a:Subcode><a:Value>c:DestinationUnreachable</a:Value></a:Subcode></a:Code><a:Reason><a:Text xml:lang="en-US">No route can be determined to reach the destination role defined by the WSAddressing To.</a:Text></a:Reason></a:Fault></a:Body></a:Envelope>"#;
        assert_eq!(
            fault(xml).unwrap(),
            "c:DestinationUnreachable: No route can be determined to reach the destination role defined by the WSAddressing To."
        );
        assert!(fault(&fixture("AMT_GeneralSettings.Get.xml")).is_none());
    }
}
