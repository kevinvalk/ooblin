//! The certificates AMT keeps, and how its TLS uses them. Like MeshCommander: they go in as DER, and come out as PEM.

use base64::prelude::{BASE64_STANDARD, Engine as _};
use instant_xml::ToXml;
use serde::Serialize;
use ts_rs::TS;

use crate::wsman::method;
use crate::{Client, Error, Result};

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Certificate {
    pub id: String,
    /// The common names of its subject and issuer.
    pub name: String,
    pub issuer: String,
    /// Trusted to sign the certificates of others (mutual TLS, 802.1X), rather than one of AMT's own.
    pub trusted_root: bool,
    /// AMT's own, which stays.
    pub read_only: bool,
    /// What AMT's TLS shows us.
    pub in_use: bool,
}

/// AMT's TLS, for the network or software on the device itself (`local`, through LMS): whether the other side must
/// show a certificate too, and whether it takes connections without TLS, where AMT supports that.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Tls {
    pub local: bool,
    pub enabled: bool,
    pub mutual: bool,
    pub plain_too: bool,
    pub plain_supported: bool,
}

impl Client {
    pub async fn certificates(&mut self) -> Result<Vec<Certificate>> {
        let contexts = self.enumerate("AMT_TLSCredentialContext").await?;
        // A context's certificate is its reference's last line, the selector's value.
        let in_use: Vec<_> =
            contexts.iter().filter_map(|context| context.get("ElementInContext")?.lines().last()).collect();
        let certificates = self.enumerate("AMT_PublicKeyCertificate").await?;
        Ok(certificates
            .iter()
            .map(|certificate| Certificate {
                id: certificate.text("InstanceID"),
                name: common_name(&certificate.text("Subject")),
                issuer: common_name(&certificate.text("Issuer")),
                // Sic, AMT's spelling.
                trusted_root: certificate.get("TrustedRootCertficate") == Some("true"),
                read_only: certificate.get("ReadOnlyCertificate") == Some("true"),
                in_use: in_use.contains(&&*certificate.text("InstanceID")),
            })
            .collect())
    }

    /// The network's first.
    pub async fn tls(&mut self) -> Result<Vec<Tls>> {
        let mut settings = self.enumerate("AMT_TLSSettingData").await?;
        // AMT names them "Intel(r) AMT 802.3 TLS Settings" and "Intel(r) AMT LMS TLS Settings".
        settings.sort_by_key(|settings| settings.text("InstanceID").contains("LMS"));
        let tls = settings.iter().map(|settings| {
            let on = |name| settings.get(name) == Some("true");
            Tls {
                local: settings.text("InstanceID").contains("LMS"),
                enabled: on("Enabled"),
                mutual: on("MutualAuthentication"),
                plain_too: on("AcceptNonSecureConnections"),
                plain_supported: on("NonSecureConnectionsSupported"),
            }
        });
        Ok(tls.collect())
    }

    /// Adds a certificate, or a `trusted_root` to trust the certificates it signs, from a PEM or DER file's contents.
    pub async fn add_certificate(&mut self, file: &[u8], trusted_root: bool) -> Result<()> {
        let der = match std::str::from_utf8(file).ok().and_then(pem_body) {
            Some(body) => BASE64_STANDARD.decode(body).map_err(|_| Error::Protocol("this PEM isn't base64".into()))?,
            None => file.to_vec(),
        };
        let blob = &BASE64_STANDARD.encode(der);
        match trusted_root {
            true => self.invoke(&AddTrustedRootCertificate { CertificateBlob: blob }).await?,
            false => self.invoke(&AddCertificate { CertificateBlob: blob }).await?,
        };
        Ok(())
    }

    /// The certificate with this `id`, as PEM.
    pub async fn certificate(&mut self, id: &str) -> Result<String> {
        let certificates = self.enumerate("AMT_PublicKeyCertificate").await?;
        let certificate = certificates.iter().find(|certificate| certificate.get("InstanceID") == Some(id));
        let certificate = certificate.ok_or_else(|| Error::Protocol(format!("the device has no certificate {id}")))?;
        let der = certificate.text("X509Certificate");
        let lines: Vec<_> = der.as_bytes().chunks(64).map(String::from_utf8_lossy).collect();
        Ok(format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n")))
    }

    /// AMT refuses one that's in use.
    pub async fn delete_certificate(&mut self, id: &str) -> Result<()> {
        self.delete("AMT_PublicKeyCertificate", id).await
    }
}

/// The `CN` of a distinguished name as AMT writes it (`C=US,…,CN=Intel® AMT …`), or all of it.
fn common_name(name: &str) -> String {
    name.split(',').find_map(|part| part.trim().strip_prefix("CN=")).unwrap_or(name).to_owned()
}

/// The base64 between a PEM's first BEGIN and END lines.
fn pem_body(pem: &str) -> Option<String> {
    let (_, rest) = pem.split_once("-----BEGIN ")?;
    let (_, rest) = rest.split_once("-----")?;
    let (body, _) = rest.split_once("-----END ")?;
    Some(body.split_whitespace().collect())
}

// From go-wsman-messages' `amt/publickey`, Copyright Intel Corporation, Apache-2.0.

const PUBLIC_KEY_MANAGEMENT: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_PublicKeyManagementService";

#[derive(ToXml)]
#[xml(rename = "AddTrustedRootCertificate_INPUT", ns(PUBLIC_KEY_MANAGEMENT))]
struct AddTrustedRootCertificate<'a> {
    CertificateBlob: &'a str,
}

method!(AddTrustedRootCertificate<'_>, PUBLIC_KEY_MANAGEMENT);

#[derive(ToXml)]
#[xml(rename = "AddCertificate_INPUT", ns(PUBLIC_KEY_MANAGEMENT))]
struct AddCertificate<'a> {
    CertificateBlob: &'a str,
}

method!(AddCertificate<'_>, PUBLIC_KEY_MANAGEMENT);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsman::{self, body_meaning, meaning};

    #[test]
    fn adds_a_trusted_root_like_go_wsman_messages() {
        // From go-wsman-messages' `publickey/managementservice_test.go`, with a shorter blob.
        let expected = r#"<h:AddTrustedRootCertificate_INPUT xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_PublicKeyManagementService"><h:CertificateBlob>MIIB</h:CertificateBlob></h:AddTrustedRootCertificate_INPUT>"#;
        let request = wsman::invoke(&AddTrustedRootCertificate { CertificateBlob: "MIIB" }, None, 0);
        assert_eq!(body_meaning(&request), meaning(expected));
    }

    #[test]
    fn reads_pem_and_names() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\nMIIB\nAAAA\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_body(pem).as_deref(), Some("MIIBAAAA"));
        assert_eq!(
            common_name("C=US,O=Intel Corporation,CN=Intel® AMT self-signed certificate"),
            "Intel® AMT self-signed certificate"
        );
    }
}
