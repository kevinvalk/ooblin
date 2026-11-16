use instant_xml::ToXml;

use super::Client;
use super::wsman::{Instance, method};
use crate::net::{Redirection, RedirectionSession};
use crate::{Consent, Error, RemoteAccess, Result};

impl Consent {
    fn opt_in_required(self) -> u32 {
        match self {
            Consent::None => 0,
            Consent::Kvm => 1,
            Consent::All => u32::MAX,
        }
    }
}

// NOTE: We switch redirection the way Intel's console does (`SetFeatures`): KVM has its own switch, IDER and SOL share
// the one of the redirection service, and all of them need its listener on 16995, which is on when any of them is.
impl Client {
    pub async fn remote_access(&mut self) -> Result<RemoteAccess> {
        let kvm = self.kvm_access_point().await?;
        let service = self.get("AMT_RedirectionService").await?;
        let opt_in = self.get("IPS_OptInService").await?;

        let is_listening = service.get("ListenerEnabled") == Some("true");
        let state = service.parse::<u16>("EnabledState").unwrap_or(32768); // From 32768 on, IDER is bit 0, SOL bit 1.

        Ok(RemoteAccess {
            is_kvm: kvm.map(|kvm| is_listening && is_enabled(&kvm)),
            is_sol: is_listening && state & 2 != 0,
            is_ider: is_listening && state & 1 != 0,
            consent: match opt_in.get("OptInRequired") {
                Some("0") => Consent::None,
                Some("1") => Consent::Kvm,
                _ => Consent::All,
            },
            is_consent_fixed: opt_in.get("CanModifyOptInPolicy") != Some("1"),
        })
    }

    /// We only change what differs from `access`, and the listener follows: it is on when anything is.
    pub(super) async fn set_remote_access(&mut self, access: &RemoteAccess) -> Result<()> {
        if let (Some(is_on), Some(kvm)) = (access.is_kvm, self.kvm_access_point().await?)
            && is_enabled(&kvm) != is_on
        {
            self.invoke(&KvmStateChange { RequestedState: if is_on { 2 } else { 3 } }).await?;
        }

        let mut service = self.get("AMT_RedirectionService").await?;
        let state = 32768 | u16::from(access.is_ider) | u16::from(access.is_sol) << 1;
        if service.parse("EnabledState") != Some(state) {
            self.invoke(&RedirectionStateChange { RequestedState: state }).await?;
            service.set("EnabledState", state);
        }

        let is_listening = access.is_kvm == Some(true) || access.is_sol || access.is_ider;
        if service.parse("ListenerEnabled") != Some(is_listening) {
            service.set("ListenerEnabled", is_listening);
            self.put("AMT_RedirectionService", &service).await?;
        }

        let mut opt_in = self.get("IPS_OptInService").await?;
        if !access.is_consent_fixed && opt_in.parse("OptInRequired") != Some(access.consent.opt_in_required()) {
            opt_in.set("OptInRequired", access.consent.opt_in_required());
            self.put("IPS_OptInService", &opt_in).await?;
        }

        Ok(())
    }

    /// We only open a session for what is on: what is off stays off until the user turns it on, see
    /// `set_remote_access`.
    pub(crate) async fn redirect(&mut self, session: RedirectionSession) -> Result<Redirection> {
        let access = self.remote_access().await?;
        let is_on = match session {
            RedirectionSession::Kvm => access.is_kvm == Some(true),
            RedirectionSession::Sol => access.is_sol,
            RedirectionSession::Ider => access.is_ider,
        };
        if !is_on {
            return Err(Error::Off);
        }

        let is_consent_required = match access.consent {
            Consent::None => false,
            Consent::Kvm => session == RedirectionSession::Kvm,
            Consent::All => true,
        };
        if is_consent_required {
            // Like MeshCommander, we go on when consent was given (3) or is in use (4).
            let opt_in = self.get("IPS_OptInService").await?;
            if !matches!(opt_in.get("OptInState"), Some("3" | "4")) {
                return Err(Error::Consent);
            }
        }

        Redirection::open(&self.target, self.fingerprint(), session).await
    }

    /// Has the device show a code on its screen for someone there to read out, unless it is showing one already
    /// (requested, 1, or displayed, 2). The code stays for the display timeout, five minutes by default.
    pub async fn start_consent(&mut self) -> Result<()> {
        let opt_in = self.get("IPS_OptInService").await?;
        if !matches!(opt_in.get("OptInState"), Some("1" | "2")) {
            self.invoke(&StartOptIn).await?;
        }

        Ok(())
    }

    /// Gives the device the code that was read out, after which it takes redirection sessions until the last one ends.
    pub async fn send_consent(&mut self, code: u32) -> Result<()> {
        match self.invoke(&SendOptInCode { OptInCode: code }).await {
            Err(Error::Method { code, .. }) if code == "2066" => Err(Error::WrongCode), // PT_STATUS_INVALID_CREDENTIALS
            sent => sent.map(drop),
        }
    }

    /// Takes the code off the device's screen.
    pub async fn cancel_consent(&mut self) -> Result<()> {
        self.invoke(&CancelOptIn).await.map(drop)
    }

    /// `None` for a device without KVM, which answers with a fault as it does for any class it does not have.
    pub(crate) async fn kvm_access_point(&mut self) -> Result<Option<Instance>> {
        match self.get("CIM_KVMRedirectionSAP").await {
            Err(Error::Fault(_) | Error::Unsupported | Error::Http(_)) => Ok(None),
            kvm => kvm.map(Some),
        }
    }
}

/// KVM is on when its access point is enabled (2) or enabled but idle (6).
fn is_enabled(kvm: &Instance) -> bool {
    matches!(kvm.get("EnabledState"), Some("2" | "6"))
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

method!(KvmStateChange, KVM_REDIRECTION, "RequestStateChange");

#[derive(ToXml)]
#[xml(rename = "RequestStateChange_INPUT", ns(REDIRECTION_SERVICE))]
struct RedirectionStateChange {
    RequestedState: u16,
}

method!(RedirectionStateChange, REDIRECTION_SERVICE, "RequestStateChange");

// The inputs below are go-wsman-messages' (`ips/optin`), Copyright Intel Corporation, Apache-2.0.

const OPT_IN_SERVICE: &str = "http://intel.com/wbem/wscim/1/ips-schema/1/IPS_OptInService";

#[derive(ToXml)]
#[xml(rename = "StartOptIn_INPUT", ns(OPT_IN_SERVICE))]
struct StartOptIn;

method!(StartOptIn, OPT_IN_SERVICE);

#[derive(ToXml)]
#[xml(rename = "SendOptInCode_INPUT", ns(OPT_IN_SERVICE))]
struct SendOptInCode {
    OptInCode: u32,
}

method!(SendOptInCode, OPT_IN_SERVICE);

#[derive(ToXml)]
#[xml(rename = "CancelOptIn_INPUT", ns(OPT_IN_SERVICE))]
struct CancelOptIn;

method!(CancelOptIn, OPT_IN_SERVICE);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amt::wsman::{self, body_meaning, meaning};

    #[test]
    fn turns_redirection_on_like_go_wsman_messages() {
        // From go-wsman-messages' `base_test.go` and `service_test.go`.
        let expected = r#"<h:RequestStateChange_INPUT xmlns:h="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_KVMRedirectionSAP"><h:RequestedState>2</h:RequestedState></h:RequestStateChange_INPUT>"#;
        let request = wsman::invoke(&KvmStateChange { RequestedState: 2 }, None, 0);
        assert_eq!(body_meaning(&request), meaning(expected));

        let expected = r#"<h:RequestStateChange_INPUT xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_RedirectionService"><h:RequestedState>32771</h:RequestedState></h:RequestStateChange_INPUT>"#;
        let request = wsman::invoke(&RedirectionStateChange { RequestedState: 32771 }, None, 0);
        assert_eq!(body_meaning(&request), meaning(expected));

        // We send an instance back whole as we read it, with what we changed.
        let expected = r#"<h:AMT_RedirectionService xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_RedirectionService"><h:CreationClassName>AMT_RedirectionService</h:CreationClassName><h:ElementName>Intel(r) AMT Redirection Service</h:ElementName><h:EnabledState>32771</h:EnabledState><h:ListenerEnabled>true</h:ListenerEnabled><h:Name>Intel(r) AMT Redirection Service</h:Name><h:SystemCreationClassName>CIM_ComputerSystem</h:SystemCreationClassName><h:SystemName>Intel(r) AMT</h:SystemName></h:AMT_RedirectionService>"#;
        let mut service = Instance {
            class: "AMT_RedirectionService".into(),
            properties: [
                ("CreationClassName", "AMT_RedirectionService"),
                ("ElementName", "Intel(r) AMT Redirection Service"),
                ("EnabledState", "32771"),
                ("ListenerEnabled", "false"),
                ("Name", "Intel(r) AMT Redirection Service"),
                ("SystemCreationClassName", "CIM_ComputerSystem"),
                ("SystemName", "Intel(r) AMT"),
            ]
            .map(|(name, value)| (name.into(), value.into()))
            .into(),
        };
        service.set("ListenerEnabled", true);
        let request = wsman::put("AMT_RedirectionService", &[], &service, 0);
        assert_eq!(body_meaning(&request), meaning(expected));
    }

    #[test]
    fn asks_for_consent_like_go_wsman_messages() {
        // From go-wsman-messages' `optin/service_test.go`.
        let expected = r#"<h:SendOptInCode_INPUT xmlns:h="http://intel.com/wbem/wscim/1/ips-schema/1/IPS_OptInService"><h:OptInCode>1</h:OptInCode></h:SendOptInCode_INPUT>"#;
        assert_eq!(body_meaning(&wsman::invoke(&SendOptInCode { OptInCode: 1 }, None, 0)), meaning(expected));
        let expected = r#"<h:StartOptIn_INPUT xmlns:h="http://intel.com/wbem/wscim/1/ips-schema/1/IPS_OptInService"></h:StartOptIn_INPUT>"#;
        assert_eq!(body_meaning(&wsman::invoke(&StartOptIn, None, 0)), meaning(expected));
    }
}
