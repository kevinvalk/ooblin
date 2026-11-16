use std::net::{IpAddr, Ipv4Addr};

use md5::{Digest, Md5};
use roxmltree::Document;
use serde_json::Value;

// NOTE: A crawl holds more than we would like to share: serial numbers, addresses, names, certificates. We swap those
// values for fakes of the same shape (digits stay digits, hex stays hex, IPv4 lands in 192.0.2.0/24), derived from the
// value itself so that the same original gets the same fake in every file. The fields cannot list everything, so IPv4
// addresses inside any longer text (the access logs of AMT, say) go too. The crawl of an AMT is XML, the Redfish of a
// BMC is JSON.
const SENSITIVE_XML: &[&str] = &[
    "SerialNumber",
    "MACAddress",
    "PermanentAddress",
    "NetworkAddresses",
    "IPAddress",
    "IPv4Address",
    "IPv6Address",
    "CurrentAddress",
    "DefaultGateway",
    "DefaultRouter",
    "PrimaryDNS",
    "SecondaryDNS",
    "CurrentPrimaryDNS",
    "CurrentSecondaryDNS",
    "HostName",
    "DomainName",
    "HostOSFQDN",
    "DhcpDNSSuffix",
    "TrustedDNSSuffix",
    "ProvisioningServerOTP",
    "UUID",
    "OtherIdentifyingInfo",
    "PlatformGUID",
    "SSID",
    "ProfileName",
    "X509Certificate",
    "PublicKey",
];

/// The names Redfish has for the same, and its secrets (which the X10 gives as `"null"`, or in clear).
const SENSITIVE_JSON: &[&str] = &[
    "SerialNumber",
    "BoardSerialNumber",
    "UUID",
    "GUID",
    "MACAddress",
    "PermanentMACAddress",
    "Address",
    "Gateway",
    "IPv6DefaultGateway",
    "NameServers",
    "StaticNameServers",
    "HostName",
    "FQDN",
    "UserName",
    "AssetTag",
    "PrimaryNTPServer",
    "SecondaryNTPServer",
    "ROCommunity",
    "RWCommunity",
    "Password",
    "RadiusSecret",
    "SmtpPassword",
];

pub fn scrub(xml: &str) -> String {
    let Ok(document) = Document::parse(xml) else {
        return xml.to_owned();
    };

    let texts = document.descendants().filter(|node| node.is_text()).map(|node| {
        let is_sensitive = node.parent().is_some_and(|parent| SENSITIVE_XML.contains(&parent.tag_name().name()));
        (node.range(), is_sensitive)
    });
    let texts: Vec<_> = texts.collect();

    // We go back to front, so that the ranges still ahead of us stay valid.
    let mut xml = xml.to_owned();
    for (range, is_sensitive) in texts.into_iter().rev() {
        let text = &xml[range.clone()];
        let scrubbed = if is_sensitive { fake(text) } else { addresses(text) };
        xml.replace_range(range, &scrubbed);
    }

    xml
}

/// Gives every string under a name of `SENSITIVE_JSON`, alone or in a list, a fake.
pub fn json(value: &mut Value) {
    fn walk(value: &mut Value, is_sensitive: bool) {
        match value {
            Value::Object(fields) => {
                fields.iter_mut().for_each(|(name, value)| walk(value, SENSITIVE_JSON.contains(&&**name)))
            }
            Value::Array(values) => values.iter_mut().for_each(|value| walk(value, is_sensitive)),
            Value::String(text) => *text = if is_sensitive { fake(text) } else { addresses(text) },
            _ => {}
        }
    }

    walk(value, false);
}

/// Fakes the IPv4 addresses inside a longer text. We keep a field that is just an address (a subnet mask, say), because
/// that is a setting and not a trace.
fn addresses(text: &str) -> String {
    if text.parse::<Ipv4Addr>().is_ok() {
        return text.to_owned();
    }

    let words = text.split(|character: char| !character.is_ascii_digit() && character != '.');
    let found: Vec<_> = words.filter(|word| word.parse::<Ipv4Addr>().is_ok()).collect();

    found.into_iter().fold(text.to_owned(), |text, address| text.replace(address, &fake(address)))
}

pub(crate) fn fake(value: &str) -> String {
    // An unspecified address is no trace of one, and it only says "no address" as it is.
    if value.parse::<IpAddr>().is_ok_and(|address| address.is_unspecified()) {
        return value.to_owned();
    }

    let hash = Md5::digest(value);
    if value.parse::<Ipv4Addr>().is_ok() {
        return format!("192.0.2.{}", 1 + hash[0] % 254);
    }

    let mut bytes = hash.iter().cycle();
    value
        .chars()
        .map(|character| {
            let byte = *bytes.next().unwrap();
            match character {
                '0'..='9' => char::from(b'0' + byte % 10),
                'a'..='f' => char::from(b'a' + byte % 6),
                'A'..='F' => char::from(b'A' + byte % 6),
                'g'..='z' => char::from(b'g' + byte % 20),
                'G'..='Z' => char::from(b'G' + byte % 20),
                other => other,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::scrub;

    #[test]
    fn keeps_the_shape_and_hides_the_value() {
        let xml = r#"<a:B xmlns:a="x"><a:SerialNumber>JRQN0243007J</a:SerialNumber><a:IPAddress>10.1.2.3</a:IPAddress><a:Model>NUC</a:Model><a:AccessLog>10/18/2026, 10:39:29, 192.168.0.126</a:AccessLog><a:SubnetMask>255.255.192.0</a:SubnetMask></a:B>"#;
        let scrubbed = scrub(xml);
        assert!(
            !scrubbed.contains("JRQN0243007J") && !scrubbed.contains("10.1.2.3") && !scrubbed.contains("192.168.0.126")
        );
        assert!(scrubbed.contains("<a:AccessLog>10/18/2026, 10:39:29, 192.0.2.") && scrubbed.contains("255.255.192.0"));
        assert!(scrubbed.contains("<a:Model>NUC</a:Model>") && scrubbed.contains("<a:IPAddress>192.0.2."));
        let serial = scrubbed.split("<a:SerialNumber>").nth(1).unwrap().split('<').next().unwrap();
        assert!(serial.len() == 12 && serial[4..11].chars().all(|c| c.is_ascii_digit()), "{serial}");
        assert_eq!(scrubbed, scrub(xml), "the same value always gets the same fake");
    }

    #[test]
    fn keeps_the_shape_of_json_and_hides_the_value() {
        let mut value = json!({
            "UUID": "00000000-0000-0000-0000-0CC47AC9FEB5",
            "IPv4Addresses": [{"Address": "10.1.2.3", "SubnetMask": "255.255.255.0"}],
            "NameServers": ["10.1.2.3", ""],
            "IPv6DefaultGateway": "0:0:0:0:0:0:0:0",
            "Message": "[ OEM ] login from 10.1.2.3",
            "Model": "ASPEED",
        });
        super::json(&mut value);
        let address = value["IPv4Addresses"][0]["Address"].as_str().unwrap().to_owned();
        assert!(address.starts_with("192.0.2."), "{address}");
        assert_eq!(value["NameServers"], json!([address, ""]), "the same value always gets the same fake");
        assert_eq!(value["Message"], format!("[ OEM ] login from {address}"));
        assert_eq!(value["IPv6DefaultGateway"], "0:0:0:0:0:0:0:0", "no address stays none");
        assert_eq!(
            (&value["Model"], &value["IPv4Addresses"][0]["SubnetMask"]),
            (&json!("ASPEED"), &json!("255.255.255.0"))
        );
        let uuid = value["UUID"].as_str().unwrap();
        assert!(uuid.len() == 36 && uuid.split('-').count() == 5 && !uuid.ends_with("0CC47AC9FEB5"), "{uuid}");
    }
}
