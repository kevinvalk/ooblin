//! Turning what a redirection session needs on before opening it, the way Intel's console does (`SetFeatures`): KVM
//! has its own switch, IDER and SOL share the redirection service's, and all of them need its listener on 16995.

use instant_xml::ToXml;

use crate::net::{Redirection, Session};
use crate::wsman::Method;
use crate::{Client, Error, Result};

impl Client {
    pub(crate) async fn redirect(&mut self, session: Session) -> Result<Redirection> {
        if session == Session::Kvm {
            let kvm = self.get("CIM_KVMRedirectionSAP").await?;
            // Enabled (2) or enabled but idle (6).
            if !matches!(kvm.get("EnabledState"), Some("2" | "6")) {
                self.invoke(&KvmStateChange { RequestedState: 2 }).await?;
            }
        }
        let service = self.get("AMT_RedirectionService").await?;
        // From 32768 on, IDER is bit 0 and SOL bit 1.
        let state = service.parse("EnabledState").unwrap_or(32768);
        let wanted = state
            | match session {
                Session::Ider => 1,
                Session::Sol => 2,
                Session::Kvm => 0,
            };
        if wanted != state {
            self.invoke(&RedirectionStateChange { RequestedState: wanted }).await?;
        }
        if service.get("ListenerEnabled") != Some("true") {
            let kept = |name| service.get(name).unwrap_or_default();
            let listening = RedirectionService {
                CreationClassName: kept("CreationClassName"),
                ElementName: kept("ElementName"),
                EnabledState: wanted,
                ListenerEnabled: true,
                Name: kept("Name"),
                SystemCreationClassName: kept("SystemCreationClassName"),
                SystemName: kept("SystemName"),
            };
            self.put(REDIRECTION_SERVICE, &listening).await?;
        }
        // Like MeshCommander: someone at the device reads out a code first, for KVM (1) or everything, unless it was
        // given (3) or is in use (4). We can't ask for one yet.
        let consent = self.get("IPS_OptInService").await?;
        let required = match consent.get("OptInRequired") {
            Some("0") => false,
            Some("1") => session == Session::Kvm,
            _ => true,
        };
        if required && !matches!(consent.get("OptInState"), Some("3" | "4")) {
            return Err(Error::Protocol("this needs user consent here, which we can't ask for yet".into()));
        }
        Redirection::open(&self.target, self.fingerprint(), session).await
    }
}

// The inputs and settings below are go-wsman-messages' (`pkg/wsman/cim/kvm`, `amt/redirection`), Copyright Intel
// Corporation, Apache-2.0.

const KVM_REDIRECTION: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_KVMRedirectionSAP";
const REDIRECTION_SERVICE: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_RedirectionService";

#[derive(ToXml)]
#[xml(rename = "RequestStateChange_INPUT", ns(KVM_REDIRECTION))]
struct KvmStateChange {
    RequestedState: u16,
}

impl Method for KvmStateChange {
    const URI: &'static str = KVM_REDIRECTION;
    const NAME: &'static str = "RequestStateChange";
}

#[derive(ToXml)]
#[xml(rename = "RequestStateChange_INPUT", ns(REDIRECTION_SERVICE))]
struct RedirectionStateChange {
    RequestedState: u16,
}

impl Method for RedirectionStateChange {
    const URI: &'static str = REDIRECTION_SERVICE;
    const NAME: &'static str = "RequestStateChange";
}

#[derive(ToXml)]
#[xml(rename = "AMT_RedirectionService", ns(REDIRECTION_SERVICE))]
struct RedirectionService<'a> {
    CreationClassName: &'a str,
    ElementName: &'a str,
    EnabledState: u16,
    ListenerEnabled: bool,
    Name: &'a str,
    SystemCreationClassName: &'a str,
    SystemName: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsman::{self, body_meaning, meaning};

    #[test]
    fn turns_redirection_on_like_go_wsman_messages() {
        // From go-wsman-messages' `base_test.go` and `service_test.go`.
        let expected = r#"<h:RequestStateChange_INPUT xmlns:h="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_KVMRedirectionSAP"><h:RequestedState>2</h:RequestedState></h:RequestStateChange_INPUT>"#;
        let request = wsman::invoke(&KvmStateChange { RequestedState: 2 }, 0);
        assert_eq!(body_meaning(&request), meaning(expected));

        let expected = r#"<h:RequestStateChange_INPUT xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_RedirectionService"><h:RequestedState>32771</h:RequestedState></h:RequestStateChange_INPUT>"#;
        let request = wsman::invoke(&RedirectionStateChange { RequestedState: 32771 }, 0);
        assert_eq!(body_meaning(&request), meaning(expected));

        let expected = r#"<h:AMT_RedirectionService xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_RedirectionService"><h:CreationClassName>AMT_RedirectionService</h:CreationClassName><h:ElementName>Intel(r) AMT Redirection Service</h:ElementName><h:EnabledState>32771</h:EnabledState><h:ListenerEnabled>true</h:ListenerEnabled><h:Name>Intel(r) AMT Redirection Service</h:Name><h:SystemCreationClassName>CIM_ComputerSystem</h:SystemCreationClassName><h:SystemName>Intel(r) AMT</h:SystemName></h:AMT_RedirectionService>"#;
        let service = RedirectionService {
            CreationClassName: "AMT_RedirectionService",
            ElementName: "Intel(r) AMT Redirection Service",
            EnabledState: 32771,
            ListenerEnabled: true,
            Name: "Intel(r) AMT Redirection Service",
            SystemCreationClassName: "CIM_ComputerSystem",
            SystemName: "Intel(r) AMT",
        };
        assert_eq!(body_meaning(&wsman::put(REDIRECTION_SERVICE, &service, 0)), meaning(expected));
    }
}
