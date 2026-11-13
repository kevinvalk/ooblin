//! Crawls hold more than we'd like to share: serial numbers, addresses, names, certificates. We swap those values for
//! fakes of the same shape (digits stay digits, hex stays hex, IPv4 lands in 192.0.2.0/24), derived from the value
//! itself so the same original gets the same fake in every file. Fields can't list everything, so IPv4 addresses inside
//! any longer text (AMT's access logs, say) go too. AMT's crawls are XML, a BMC's Redfish JSON.

use std::net::Ipv4Addr;

use md5::{Digest, Md5};
use roxmltree::Document;
use serde_json::Value;

const SENSITIVE: &[&str] = &[
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

/// Redfish's names for the same, and its secrets (which the X10 gives as `"null"`, or in clear).
const JSON: &[&str] = &[
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
    let Ok(doc) = Document::parse(xml) else {
        return xml.to_owned();
    };
    let texts = doc.descendants().filter(|node| node.is_text()).map(|node| {
        let sensitive = node.parent().is_some_and(|parent| SENSITIVE.contains(&parent.tag_name().name()));
        (node.range(), sensitive)
    });
    let texts: Vec<_> = texts.collect();

    // Back to front, so the ranges still ahead of us stay valid.
    let mut xml = xml.to_owned();
    for (range, sensitive) in texts.into_iter().rev() {
        let text = &xml[range.clone()];
        let scrubbed = if sensitive { fake(text) } else { addresses(text) };
        xml.replace_range(range, &scrubbed);
    }
    xml
}

/// Strings under the names above, alone or in a list, get a fake.
pub fn json(value: &mut Value) {
    fn walk(value: &mut Value, sensitive: bool) {
        match value {
            Value::Object(fields) => fields.iter_mut().for_each(|(name, value)| walk(value, JSON.contains(&&**name))),
            Value::Array(values) => values.iter_mut().for_each(|value| walk(value, sensitive)),
            Value::String(text) => *text = if sensitive { fake(text) } else { addresses(text) },
            _ => {}
        }
    }
    walk(value, false);
}

/// IPv4 addresses inside longer text. A field that is just an address (a subnet mask, say) is a setting, not a trace.
fn addresses(text: &str) -> String {
    if text.parse::<Ipv4Addr>().is_ok() {
        return text.to_owned();
    }
    let words = text.split(|c: char| !c.is_ascii_digit() && c != '.');
    let found: Vec<_> = words.filter(|word| word.parse::<Ipv4Addr>().is_ok()).collect();
    found.into_iter().fold(text.to_owned(), |text, address| text.replace(address, &fake(address)))
}

fn fake(value: &str) -> String {
    let hash = Md5::digest(value);
    if value.parse::<Ipv4Addr>().is_ok() {
        return format!("192.0.2.{}", 1 + hash[0] % 254);
    }
    let mut bytes = hash.iter().cycle();
    value
        .chars()
        .map(|c| {
            let n = *bytes.next().unwrap();
            match c {
                '0'..='9' => char::from(b'0' + n % 10),
                'a'..='f' => char::from(b'a' + n % 6),
                'A'..='F' => char::from(b'A' + n % 6),
                'g'..='z' => char::from(b'g' + n % 20),
                'G'..='Z' => char::from(b'G' + n % 20),
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
            "Message": "[ OEM ] login from 10.1.2.3",
            "Model": "ASPEED",
        });
        super::json(&mut value);
        let address = value["IPv4Addresses"][0]["Address"].as_str().unwrap().to_owned();
        assert!(address.starts_with("192.0.2."), "{address}");
        assert_eq!(value["NameServers"], json!([address, ""]), "the same value always gets the same fake");
        assert_eq!(value["Message"], format!("[ OEM ] login from {address}"));
        assert_eq!(
            (&value["Model"], &value["IPv4Addresses"][0]["SubnetMask"]),
            (&json!("ASPEED"), &json!("255.255.255.0"))
        );
        let uuid = value["UUID"].as_str().unwrap();
        assert!(uuid.len() == 36 && uuid.split('-').count() == 5 && !uuid.ends_with("0CC47AC9FEB5"), "{uuid}");
    }
}
