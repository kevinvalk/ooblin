use instant_xml::ToXml;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::wsman::{Instance, Method, Reference};
use crate::{Client, Error, Result};

/// The power state, and what the device offers to do about it right now.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Power {
    pub state: PowerState,
    pub actions: Vec<PowerAction>,
}

/// The CIM `PowerState`s AMT reports, folded into what a person cares about and named the way they'd say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum PowerState {
    On,
    Asleep,
    Hibernating,
    Off,
    Unknown,
}

/// The power changes AMT can make. The `Soft` ones, `Sleep` and `Hibernate` ask the OS, the `To` ones power on or reset
/// into the BIOS setup, a network boot or the disk image in the IDER drive, once, or with the BIOS's screen on the serial
/// console (`UseSOL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum PowerAction {
    On,
    SoftOff,
    Off,
    SoftReset,
    Reset,
    Cycle,
    Sleep,
    Hibernate,
    Nmi,
    OnToBios,
    OnToPxe,
    OnToImage,
    OnToSerial,
    ResetToBios,
    ResetToPxe,
    ResetToImage,
    ResetToSerial,
}

impl PowerAction {
    /// Its CIM `PowerState`, as go-wsman-messages names them (8 is a hard off, the handover had it as soft).
    fn state(self) -> u8 {
        match self {
            Self::On | Self::OnToBios | Self::OnToPxe | Self::OnToImage | Self::OnToSerial => 2,
            Self::Sleep => 4,
            Self::Cycle => 5,
            Self::Hibernate => 7,
            Self::Off => 8,
            Self::Reset | Self::ResetToBios | Self::ResetToPxe | Self::ResetToImage | Self::ResetToSerial => 10,
            Self::Nmi => 11,
            Self::SoftOff => 12,
            Self::SoftReset => 14,
        }
    }

    fn from_state(state: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.state().to_string() == state)
    }

    const ALL: [Self; 9] = [
        Self::On,
        Self::SoftOff,
        Self::Off,
        Self::SoftReset,
        Self::Reset,
        Self::Cycle,
        Self::Sleep,
        Self::Hibernate,
        Self::Nmi,
    ];
}

impl Client {
    pub async fn power(&mut self) -> Result<Power> {
        let associations = self.enumerate("CIM_ServiceAvailableToElement").await?;
        let association = associations.iter().find(|association| association.get("PowerState").is_some());
        let association = association.ok_or_else(|| Error::Protocol("no PowerState on the device".into()))?;
        let state = match association.get("PowerState") {
            Some("2") => PowerState::On,
            Some("3" | "4") => PowerState::Asleep,
            Some("7") => PowerState::Hibernating,
            Some("6" | "8" | "13") => PowerState::Off,
            _ => PowerState::Unknown,
        };
        let mut actions: Vec<_> =
            association.all("AvailableRequestedPowerStates").filter_map(PowerAction::from_state).collect();
        // Like Intel's console: into the BIOS, PXE or IDER, or on SOL, when it says it can.
        let boot = self.get("AMT_BootCapabilities").await?;
        let can = |capability| boot.get(capability) == Some("true");
        for (via, bios, pxe, image, serial) in [
            (
                PowerAction::On,
                PowerAction::OnToBios,
                PowerAction::OnToPxe,
                PowerAction::OnToImage,
                PowerAction::OnToSerial,
            ),
            (
                PowerAction::Reset,
                PowerAction::ResetToBios,
                PowerAction::ResetToPxe,
                PowerAction::ResetToImage,
                PowerAction::ResetToSerial,
            ),
        ] {
            if actions.contains(&via) {
                let targets = [("BIOSSetup", bios), ("ForcePXEBoot", pxe), ("IDER", image), ("SOL", serial)];
                actions
                    .extend(targets.into_iter().filter(|&(capability, _)| can(capability)).map(|(_, action)| action));
            }
        }
        Ok(Power { state, actions })
    }

    pub async fn set_power(&mut self, action: PowerAction) -> Result<()> {
        match action {
            PowerAction::OnToBios | PowerAction::ResetToBios => self.boot_once(Boot::Bios).await?,
            PowerAction::OnToPxe | PowerAction::ResetToPxe => self.boot_once(Boot::Pxe).await?,
            PowerAction::OnToImage | PowerAction::ResetToImage => self.boot_once(Boot::Image).await?,
            PowerAction::OnToSerial | PowerAction::ResetToSerial => self.boot_once(Boot::Serial).await?,
            _ => {}
        }
        self.invoke(&RequestPowerStateChange { PowerState: action.state(), ManagedElement: SYSTEM }).await?;
        Ok(())
    }

    /// Sets up the next boot like Intel's console does (`internal/usecase/devices/power.go`): clear the boot order,
    /// put the boot settings, make that configuration the next boot's, and boot from PXE if that's where.
    async fn boot_once(&mut self, boot: Boot) -> Result<()> {
        let current = self.get("AMT_BootSettingData").await?;
        self.invoke(&ChangeBootOrder { Source: None }).await?;
        self.put("AMT_BootSettingData", &BootSettingData::once(&current, boot)).await?;
        let configuration = Reference {
            class: "CIM_BootConfigSetting",
            selectors: &[("InstanceID", "Intel(r) AMT: Boot Configuration 0")],
        };
        self.invoke(&SetBootConfigRole { BootConfigSetting: configuration, Role: 1 }).await?;
        if boot == Boot::Pxe {
            let pxe = Reference {
                class: "CIM_BootSourceSetting",
                selectors: &[("InstanceID", "Intel(r) AMT: Force PXE Boot")],
            };
            self.invoke(&ChangeBootOrder { Source: Some(pxe) }).await?;
        }
        Ok(())
    }
}

// The inputs and settings below are go-wsman-messages' (`pkg/wsman/cim/power`, `cim/boot`, `amt/boot`), Copyright Intel
// Corporation, Apache-2.0.

/// One-Click Recovery (booting a recovery image) and remote platform erase, which the BIOS has to allow as well: `None`
/// where the device can't.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Recovery {
    pub one_click: Option<bool>,
    pub platform_erase: Option<bool>,
}

impl Client {
    /// As Intel's console reads them: the boot service's state from 32768 on has One-Click Recovery as bit 0 and
    /// platform erase as bit 1.
    pub async fn recovery(&mut self) -> Result<Recovery> {
        let capabilities = self.get("AMT_BootCapabilities").await?;
        let state = self.get("CIM_BootService").await?.parse::<u16>("EnabledState").unwrap_or(0);
        let on = |bit| state >= 32768 && state & bit != 0;
        let can = |name| capabilities.get(name) == Some("true");
        let one_click = can("ForceUEFIHTTPSBoot") || can("ForceWinREBoot") || can("ForceUEFIPBABoot");
        let platform_erase = capabilities.parse::<u32>("PlatformErase").unwrap_or(0) & 1 != 0;
        Ok(Recovery { one_click: one_click.then(|| on(1)), platform_erase: platform_erase.then(|| on(2)) })
    }

    /// Like MeshCommander, a state the device didn't take is one its BIOS doesn't allow.
    pub async fn set_recovery(&mut self, wanted: &Recovery) -> Result<()> {
        let state =
            32768 | u16::from(wanted.one_click == Some(true)) | u16::from(wanted.platform_erase == Some(true)) << 1;
        self.invoke(&BootStateChange { RequestedState: state }).await?;
        if self.get("CIM_BootService").await?.parse("EnabledState") != Some(state) {
            return Err(Error::Protocol("the device didn't take it, its BIOS may not allow it".into()));
        }
        Ok(())
    }
}

const POWER_MANAGEMENT: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_PowerManagementService";
const BOOT_CONFIG: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_BootConfigSetting";
const BOOT_SERVICE: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_BootService";
const BOOT_SETTINGS: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_BootSettingData";

/// The host, the one system AMT changes the power of.
const SYSTEM: Reference = Reference {
    class: "CIM_ComputerSystem",
    selectors: &[("CreationClassName", "CIM_ComputerSystem"), ("Name", "ManagedSystem")],
};

#[derive(ToXml)]
#[xml(rename = "RequestPowerStateChange_INPUT", ns(POWER_MANAGEMENT))]
struct RequestPowerStateChange<'a> {
    PowerState: u8,
    ManagedElement: Reference<'a>,
}

impl Method for RequestPowerStateChange<'_> {
    const URI: &'static str = POWER_MANAGEMENT;
    const NAME: &'static str = "RequestPowerStateChange";
}

/// Without a `Source`, it clears the boot order.
#[derive(ToXml)]
#[xml(rename = "ChangeBootOrder_INPUT", ns(BOOT_CONFIG))]
struct ChangeBootOrder<'a> {
    Source: Option<Reference<'a>>,
}

impl Method for ChangeBootOrder<'_> {
    const URI: &'static str = BOOT_CONFIG;
    const NAME: &'static str = "ChangeBootOrder";
}

#[derive(ToXml)]
#[xml(rename = "SetBootConfigRole_INPUT", ns(BOOT_SERVICE))]
struct SetBootConfigRole<'a> {
    BootConfigSetting: Reference<'a>,
    Role: u8,
}

impl Method for SetBootConfigRole<'_> {
    const URI: &'static str = BOOT_SERVICE;
    const NAME: &'static str = "SetBootConfigRole";
}

/// Exactly the settings AMT takes back.
#[derive(ToXml)]
#[xml(rename = "RequestStateChange_INPUT", ns(BOOT_SERVICE))]
struct BootStateChange {
    RequestedState: u16,
}

impl Method for BootStateChange {
    const URI: &'static str = BOOT_SERVICE;
    const NAME: &'static str = "RequestStateChange";
}

#[derive(ToXml)]
#[xml(rename = "AMT_BootSettingData", ns(BOOT_SETTINGS))]
struct BootSettingData<'a> {
    BIOSPause: bool,
    BIOSSetup: bool,
    BootMediaIndex: u8,
    ConfigurationDataReset: bool,
    ElementName: &'a str,
    EnforceSecureBoot: bool,
    FirmwareVerbosity: u8,
    ForcedProgressEvents: bool,
    IDERBootDevice: u8,
    InstanceID: &'a str,
    LockKeyboard: bool,
    LockPowerButton: bool,
    LockResetButton: bool,
    LockSleepButton: bool,
    OwningEntity: &'a str,
    PlatformErase: bool,
    RSEPassword: &'a str,
    ReflashBIOS: bool,
    SecureErase: bool,
    UefiBootParametersArray: &'a str,
    UefiBootNumberOfParams: u8,
    UseIDER: bool,
    UseSOL: bool,
    UseSafeMode: bool,
    UserPasswordBypass: bool,
}

/// Where to boot once: the BIOS setup, the network, the disk image in the IDER drive, or as usual on the serial console.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Boot {
    Bios,
    Pxe,
    Image,
    Serial,
}

impl<'a> BootSettingData<'a> {
    /// Intel's console keeps the device's own names and secure boot, clears the rest, and says where to boot: the
    /// BIOS setup, or IDER's CD-ROM (its device 1), or that the BIOS's screen goes to SOL.
    fn once(current: &'a Instance, boot: Boot) -> Self {
        let kept = |name| current.get(name).unwrap_or_default();
        Self {
            BIOSPause: false,
            BIOSSetup: boot == Boot::Bios,
            BootMediaIndex: 0,
            ConfigurationDataReset: false,
            ElementName: kept("ElementName"),
            EnforceSecureBoot: current.get("EnforceSecureBoot") == Some("true"),
            FirmwareVerbosity: 0,
            ForcedProgressEvents: false,
            IDERBootDevice: u8::from(boot == Boot::Image),
            InstanceID: kept("InstanceID"),
            LockKeyboard: false,
            LockPowerButton: false,
            LockResetButton: false,
            LockSleepButton: false,
            OwningEntity: kept("OwningEntity"),
            PlatformErase: false,
            RSEPassword: "",
            ReflashBIOS: false,
            SecureErase: false,
            UefiBootParametersArray: "",
            UefiBootNumberOfParams: 0,
            UseIDER: boot == Boot::Image,
            UseSOL: boot == Boot::Serial,
            UseSafeMode: false,
            UserPasswordBypass: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsman::{self, body_meaning, meaning};

    #[test]
    fn requests_a_power_state_change_like_go_wsman_messages() {
        // `RequestPowerStateChangeBODY` from go-wsman-messages' own tests, for a hard power off.
        let expected = r#"<h:RequestPowerStateChange_INPUT xmlns:h="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_PowerManagementService"><h:PowerState>8</h:PowerState><h:ManagedElement><Address xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing">http://schemas.xmlsoap.org/ws/2004/08/addressing</Address><ReferenceParameters xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing"><ResourceURI xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd">http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ComputerSystem</ResourceURI><SelectorSet xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><Selector Name="CreationClassName">CIM_ComputerSystem</Selector><Selector Name="Name">ManagedSystem</Selector></SelectorSet></ReferenceParameters></h:ManagedElement></h:RequestPowerStateChange_INPUT>"#;
        let request = wsman::invoke(&RequestPowerStateChange { PowerState: 8, ManagedElement: SYSTEM }, None, 0);
        assert_eq!(body_meaning(&request), meaning(expected));
    }

    #[test]
    fn puts_boot_settings_like_go_wsman_messages() {
        // From go-wsman-messages' `settingdata_test.go`.
        let expected = r#"<h:AMT_BootSettingData xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_BootSettingData"><h:BIOSPause>false</h:BIOSPause><h:BIOSSetup>false</h:BIOSSetup><h:BootMediaIndex>0</h:BootMediaIndex><h:ConfigurationDataReset>false</h:ConfigurationDataReset><h:ElementName>Intel(r) AMT Boot Configuration Settings</h:ElementName><h:EnforceSecureBoot>false</h:EnforceSecureBoot><h:FirmwareVerbosity>0</h:FirmwareVerbosity><h:ForcedProgressEvents>false</h:ForcedProgressEvents><h:IDERBootDevice>0</h:IDERBootDevice><h:InstanceID>Intel(r) AMT:BootSettingData 0</h:InstanceID><h:LockKeyboard>false</h:LockKeyboard><h:LockPowerButton>false</h:LockPowerButton><h:LockResetButton>false</h:LockResetButton><h:LockSleepButton>false</h:LockSleepButton><h:OwningEntity>Intel(r) AMT</h:OwningEntity><h:PlatformErase>false</h:PlatformErase><h:RSEPassword></h:RSEPassword><h:ReflashBIOS>false</h:ReflashBIOS><h:SecureErase>false</h:SecureErase><h:UefiBootParametersArray></h:UefiBootParametersArray><h:UefiBootNumberOfParams>0</h:UefiBootNumberOfParams><h:UseIDER>false</h:UseIDER><h:UseSOL>false</h:UseSOL><h:UseSafeMode>false</h:UseSafeMode><h:UserPasswordBypass>false</h:UserPasswordBypass></h:AMT_BootSettingData>"#;
        let current = Instance {
            class: "AMT_BootSettingData".into(),
            properties: [
                ("ElementName", "Intel(r) AMT Boot Configuration Settings"),
                ("InstanceID", "Intel(r) AMT:BootSettingData 0"),
                ("OwningEntity", "Intel(r) AMT"),
                ("EnforceSecureBoot", "false"),
            ]
            .map(|(name, value)| (name.into(), value.into()))
            .into(),
        };
        let request = wsman::put("AMT_BootSettingData", &[], &BootSettingData::once(&current, Boot::Pxe), 0);
        assert_eq!(body_meaning(&request), meaning(expected));
    }
}
