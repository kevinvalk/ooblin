use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::wsman::{self, Instance};
use crate::{Client, Error, Result};

/// The power state, and what the device offers to do about it right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
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
    #[serde(rename = "Asleep")]
    Sleep,
    #[serde(rename = "Hibernating")]
    Hibernate,
    Off,
    Unknown,
}

/// The power changes AMT can make. The `Soft` ones, `Sleep` and `Hibernate` ask the OS, the `To` ones power on or reset
/// into the BIOS setup or a network boot, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
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
    ResetToBios,
    ResetToPxe,
}

impl PowerAction {
    /// Its CIM `PowerState`, as go-wsman-messages names them (8 is a hard off, the handover had it as soft).
    fn state(self) -> u8 {
        match self {
            Self::On | Self::OnToBios | Self::OnToPxe => 2,
            Self::Sleep => 4,
            Self::Cycle => 5,
            Self::Hibernate => 7,
            Self::Off => 8,
            Self::Reset | Self::ResetToBios | Self::ResetToPxe => 10,
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
            Some("3" | "4") => PowerState::Sleep,
            Some("7") => PowerState::Hibernate,
            Some("6" | "8" | "13") => PowerState::Off,
            _ => PowerState::Unknown,
        };
        let mut actions: Vec<_> =
            association.all("AvailableRequestedPowerStates").filter_map(PowerAction::from_state).collect();
        // Like Intel's console: into the BIOS when it says it can, PXE when it says it can.
        let boot = self.get("AMT_BootCapabilities").await?;
        let can = |capability| boot.get(capability) == Some("true");
        for (via, bios, pxe) in [
            (PowerAction::On, PowerAction::OnToBios, PowerAction::OnToPxe),
            (PowerAction::Reset, PowerAction::ResetToBios, PowerAction::ResetToPxe),
        ] {
            if actions.contains(&via) {
                actions.extend(can("BIOSSetup").then_some(bios).into_iter().chain(can("ForcePXEBoot").then_some(pxe)));
            }
        }
        Ok(Power { state, actions })
    }

    pub async fn set_power(&mut self, action: PowerAction) -> Result<()> {
        match action {
            PowerAction::OnToBios | PowerAction::ResetToBios => self.boot_once(true).await?,
            PowerAction::OnToPxe | PowerAction::ResetToPxe => self.boot_once(false).await?,
            _ => {}
        }
        let input = request_power_state_change(action.state());
        self.invoke("CIM_PowerManagementService", "RequestPowerStateChange", &input).await?;
        Ok(())
    }

    /// Sets up the next boot like Intel's console does (`internal/usecase/devices/power.go`): clear the boot order,
    /// put the boot settings, make that configuration the next boot's, and boot from PXE unless it's the BIOS setup.
    async fn boot_once(&mut self, bios: bool) -> Result<()> {
        let current = self.get("AMT_BootSettingData").await?;
        self.invoke("CIM_BootConfigSetting", "ChangeBootOrder", &change_boot_order(None)).await?;
        self.put("AMT_BootSettingData", &boot_setting_data(&current, bios)).await?;
        let role = [("BootConfigSetting", BOOT_CONFIG_SETTING), ("Role", "1")];
        self.invoke(
            "CIM_BootService",
            "SetBootConfigRole",
            &wsman::input("CIM_BootService", "SetBootConfigRole", &role),
        )
        .await?;
        if !bios {
            let pxe = change_boot_order(Some("Intel(r) AMT: Force PXE Boot"));
            self.invoke("CIM_BootConfigSetting", "ChangeBootOrder", &pxe).await?;
        }
        Ok(())
    }
}

/// The boot settings go-wsman-messages puts (`pkg/wsman/amt/boot/settingdata.go`, Copyright Intel Corporation,
/// Apache-2.0), AMT takes exactly these. Intel's console keeps the device's own names and secure boot, clears the rest.
fn boot_setting_data(current: &Instance, bios: bool) -> String {
    let (name, id, owner) = (current.text("ElementName"), current.text("InstanceID"), current.text("OwningEntity"));
    let secure = current.get("EnforceSecureBoot") == Some("true");
    format!(
        r#"<h:AMT_BootSettingData xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_BootSettingData"><h:BIOSPause>false</h:BIOSPause><h:BIOSSetup>{bios}</h:BIOSSetup><h:BootMediaIndex>0</h:BootMediaIndex><h:ConfigurationDataReset>false</h:ConfigurationDataReset><h:ElementName>{name}</h:ElementName><h:EnforceSecureBoot>{secure}</h:EnforceSecureBoot><h:FirmwareVerbosity>0</h:FirmwareVerbosity><h:ForcedProgressEvents>false</h:ForcedProgressEvents><h:IDERBootDevice>0</h:IDERBootDevice><h:InstanceID>{id}</h:InstanceID><h:LockKeyboard>false</h:LockKeyboard><h:LockPowerButton>false</h:LockPowerButton><h:LockResetButton>false</h:LockResetButton><h:LockSleepButton>false</h:LockSleepButton><h:OwningEntity>{owner}</h:OwningEntity><h:PlatformErase>false</h:PlatformErase><h:RSEPassword></h:RSEPassword><h:ReflashBIOS>false</h:ReflashBIOS><h:SecureErase>false</h:SecureErase><h:UefiBootParametersArray></h:UefiBootParametersArray><h:UefiBootNumberOfParams>0</h:UefiBootNumberOfParams><h:UseIDER>false</h:UseIDER><h:UseSOL>false</h:UseSOL><h:UseSafeMode>false</h:UseSafeMode><h:UserPasswordBypass>false</h:UserPasswordBypass></h:AMT_BootSettingData>"#
    )
}

/// From go-wsman-messages (`pkg/wsman/cim/boot/configsetting.go`), Copyright Intel Corporation, Apache-2.0.
fn change_boot_order(source: Option<&str>) -> String {
    let source = source.map(|source| format!(r#"<h:Source><Address xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing">http://schemas.xmlsoap.org/ws/2004/08/addressing</Address><ReferenceParameters xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing"><ResourceURI xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd">http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_BootSourceSetting</ResourceURI><SelectorSet xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><Selector Name="InstanceID">{source}</Selector></SelectorSet></ReferenceParameters></h:Source>"#));
    format!(
        r#"<h:ChangeBootOrder_INPUT xmlns:h="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_BootConfigSetting">{}</h:ChangeBootOrder_INPUT>"#,
        source.unwrap_or_default()
    )
}

/// From go-wsman-messages (`pkg/wsman/cim/boot/service.go`), Copyright Intel Corporation, Apache-2.0.
const BOOT_CONFIG_SETTING: &str = r#"<Address xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing">http://schemas.xmlsoap.org/ws/2004/08/addressing</Address><ReferenceParameters xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing"><ResourceURI xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd">http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_BootConfigSetting</ResourceURI><SelectorSet xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><Selector Name="InstanceID">Intel(r) AMT: Boot Configuration 0</Selector></SelectorSet></ReferenceParameters>"#;

/// From go-wsman-messages (`pkg/wsman/cim/power/managementservice.go`), Copyright Intel Corporation, Apache-2.0.
fn request_power_state_change(state: u8) -> String {
    format!(
        r#"<h:RequestPowerStateChange_INPUT xmlns:h="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_PowerManagementService"><h:PowerState>{state}</h:PowerState><h:ManagedElement><Address xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing">http://schemas.xmlsoap.org/ws/2004/08/addressing</Address><ReferenceParameters xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing"><ResourceURI xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd">http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ComputerSystem</ResourceURI><SelectorSet xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><Selector Name="CreationClassName">CIM_ComputerSystem</Selector><Selector Name="Name">ManagedSystem</Selector></SelectorSet></ReferenceParameters></h:ManagedElement></h:RequestPowerStateChange_INPUT>"#
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn requests_a_power_state_change_like_go_wsman_messages() {
        // `RequestPowerStateChangeBODY` from go-wsman-messages' own tests, for a hard power off.
        let expected = r#"<h:RequestPowerStateChange_INPUT xmlns:h="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_PowerManagementService"><h:PowerState>8</h:PowerState><h:ManagedElement><Address xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing">http://schemas.xmlsoap.org/ws/2004/08/addressing</Address><ReferenceParameters xmlns="http://schemas.xmlsoap.org/ws/2004/08/addressing"><ResourceURI xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd">http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ComputerSystem</ResourceURI><SelectorSet xmlns="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><Selector Name="CreationClassName">CIM_ComputerSystem</Selector><Selector Name="Name">ManagedSystem</Selector></SelectorSet></ReferenceParameters></h:ManagedElement></h:RequestPowerStateChange_INPUT>"#;
        assert_eq!(super::request_power_state_change(8), expected);
    }

    #[test]
    fn puts_boot_settings_like_go_wsman_messages() {
        // From go-wsman-messages' `settingdata_test.go`.
        let expected = r#"<h:AMT_BootSettingData xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_BootSettingData"><h:BIOSPause>false</h:BIOSPause><h:BIOSSetup>false</h:BIOSSetup><h:BootMediaIndex>0</h:BootMediaIndex><h:ConfigurationDataReset>false</h:ConfigurationDataReset><h:ElementName>Intel(r) AMT Boot Configuration Settings</h:ElementName><h:EnforceSecureBoot>false</h:EnforceSecureBoot><h:FirmwareVerbosity>0</h:FirmwareVerbosity><h:ForcedProgressEvents>false</h:ForcedProgressEvents><h:IDERBootDevice>0</h:IDERBootDevice><h:InstanceID>Intel(r) AMT:BootSettingData 0</h:InstanceID><h:LockKeyboard>false</h:LockKeyboard><h:LockPowerButton>false</h:LockPowerButton><h:LockResetButton>false</h:LockResetButton><h:LockSleepButton>false</h:LockSleepButton><h:OwningEntity>Intel(r) AMT</h:OwningEntity><h:PlatformErase>false</h:PlatformErase><h:RSEPassword></h:RSEPassword><h:ReflashBIOS>false</h:ReflashBIOS><h:SecureErase>false</h:SecureErase><h:UefiBootParametersArray></h:UefiBootParametersArray><h:UefiBootNumberOfParams>0</h:UefiBootNumberOfParams><h:UseIDER>false</h:UseIDER><h:UseSOL>false</h:UseSOL><h:UseSafeMode>false</h:UseSafeMode><h:UserPasswordBypass>false</h:UserPasswordBypass></h:AMT_BootSettingData>"#;
        let current = crate::wsman::Instance {
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
        assert_eq!(super::boot_setting_data(&current, false), expected);
    }
}
