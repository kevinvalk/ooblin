//! What the device lets us redirect, and opening a session where it does. Like Intel's console (`SetFeatures`): KVM has
//! its own switch, IDER and SOL share the redirection service's, and all of them need its listener on 16995, which is
//! on when any of them is.

use instant_xml::ToXml;

use super::Client;
use super::wsman::{Instance, method};
use crate::net::{Redirection, Session};
use crate::{Consent, Error, RemoteAccess, Result};

impl Consent {
    /// `OptInRequired`'s value for it.
    fn required(self) -> u32 {
        match self {
            Consent::None => 0,
            Consent::Kvm => 1,
            Consent::All => u32::MAX,
        }
    }
}

impl Client {
    pub async fn remote_access(&mut self) -> Result<RemoteAccess> {
        let kvm = self.kvm_access_point().await?;
        let service = self.get("AMT_RedirectionService").await?;
        let opt_in = self.get("IPS_OptInService").await?;
        let listening = service.get("ListenerEnabled") == Some("true");
        // From 32768 on, IDER is bit 0 and SOL bit 1.
        let state = service.parse::<u16>("EnabledState").unwrap_or(32768);
        Ok(RemoteAccess {
            kvm: kvm.map(|kvm| listening && enabled(&kvm)),
            sol: listening && state & 2 != 0,
            ider: listening && state & 1 != 0,
            consent: match opt_in.get("OptInRequired") {
                Some("0") => Consent::None,
                Some("1") => Consent::Kvm,
                _ => Consent::All,
            },
            consent_fixed: opt_in.get("CanModifyOptInPolicy") != Some("1"),
        })
    }

    /// Changes what differs from `access`, the listener following: on when anything is.
    pub(super) async fn set_remote_access(&mut self, access: &RemoteAccess) -> Result<()> {
        if let (Some(on), Some(kvm)) = (access.kvm, self.kvm_access_point().await?)
            && enabled(&kvm) != on
        {
            self.invoke(&KvmStateChange { RequestedState: if on { 2 } else { 3 } }).await?;
        }
        let mut service = self.get("AMT_RedirectionService").await?;
        let state = 32768 | u16::from(access.ider) | u16::from(access.sol) << 1;
        if service.parse("EnabledState") != Some(state) {
            self.invoke(&RedirectionStateChange { RequestedState: state }).await?;
            service.set("EnabledState", state);
        }
        let listening = access.kvm == Some(true) || access.sol || access.ider;
        if service.parse("ListenerEnabled") != Some(listening) {
            service.set("ListenerEnabled", listening);
            self.put("AMT_RedirectionService", &service).await?;
        }
        let mut opt_in = self.get("IPS_OptInService").await?;
        if !access.consent_fixed && opt_in.parse("OptInRequired") != Some(access.consent.required()) {
            opt_in.set("OptInRequired", access.consent.required());
            self.put("IPS_OptInService", &opt_in).await?;
        }
        Ok(())
    }

    /// Only what's on: what's off stays so until turned on, see `set_remote_access`.
    pub(crate) async fn redirect(&mut self, session: Session) -> Result<Redirection> {
        let access = self.remote_access().await?;
        let on = match session {
            Session::Kvm => access.kvm == Some(true),
            Session::Sol => access.sol,
            Session::Ider => access.ider,
        };
        if !on {
            return Err(Error::Off);
        }
        // Like MeshCommander: unless consent was given (3) or is in use (4).
        let required = match access.consent {
            Consent::None => false,
            Consent::Kvm => session == Session::Kvm,
            Consent::All => true,
        };
        if required && !matches!(self.get("IPS_OptInService").await?.get("OptInState"), Some("3" | "4")) {
            return Err(Error::Consent);
        }
        Redirection::open(&self.target, self.fingerprint(), session).await
    }

    /// Has the device show a code on its screen, for someone there to read out, unless it's showing one already
    /// (requested, 1, or displayed, 2). It stays for the display timeout, five minutes by default.
    pub async fn start_consent(&mut self) -> Result<()> {
        if !matches!(self.get("IPS_OptInService").await?.get("OptInState"), Some("1" | "2")) {
            self.invoke(&StartOptIn).await?;
        }
        Ok(())
    }

    /// Gives the code read out, after which the device takes redirection sessions until the last one ends.
    pub async fn send_consent(&mut self, code: u32) -> Result<()> {
        match self.invoke(&SendOptInCode { OptInCode: code }).await {
            // PT_STATUS_INVALID_CREDENTIALS
            Err(Error::Method { code, .. }) if code == "2066" => Err(Error::WrongCode),
            sent => sent.map(drop),
        }
    }

    /// Takes the code off the device's screen.
    pub async fn cancel_consent(&mut self) -> Result<()> {
        self.invoke(&CancelOptIn).await.map(drop)
    }

    /// KVM's access point, which a device without KVM doesn't have: it answers with a fault, like for any class it
    /// doesn't have.
    pub(crate) async fn kvm_access_point(&mut self) -> Result<Option<Instance>> {
        match self.get("CIM_KVMRedirectionSAP").await {
            Err(Error::Fault(_) | Error::Unsupported | Error::Http(_)) => Ok(None),
            kvm => kvm.map(Some),
        }
    }
}

/// KVM is on when its access point is enabled (2) or enabled but idle (6).
fn enabled(kvm: &Instance) -> bool {
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

// From go-wsman-messages' `ips/optin`, Copyright Intel Corporation, Apache-2.0.

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

        // An instance as read goes back whole, with what we changed.
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
