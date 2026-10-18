//! Crawls hold more than we'd like to share: serial numbers, addresses, names, certificates. We swap those values for
//! fakes of the same shape (digits stay digits, hex stays hex, IPv4 lands in 192.0.2.0/24), derived from the value
//! itself so the same original gets the same fake in every file. Fields can't list everything, so IPv4 addresses inside
//! any longer text (AMT's access logs, say) go too.

use std::net::Ipv4Addr;

use md5::{Digest, Md5};
use roxmltree::Document;

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
    "PlatformGUID",
    "SSID",
    "ProfileName",
    "X509Certificate",
    "PublicKey",
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
}
