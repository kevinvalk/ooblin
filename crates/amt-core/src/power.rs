use serde::{Deserialize, Serialize};

use crate::{Client, Error, Result};

/// The CIM `PowerState`s AMT reports, folded into what a person cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PowerState {
    On,
    Sleep,
    Hibernate,
    Off,
    Unknown,
}

/// The four actions go-wsman-messages marks as verified on hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PowerAction {
    On,
    Off,
    Cycle,
    Reset,
}

impl Client {
    pub async fn power_state(&mut self) -> Result<PowerState> {
        let associations = self.enumerate("CIM_ServiceAvailableToElement").await?;
        let state = associations.iter().find_map(|association| association.get("PowerState"));
        Ok(match state.ok_or_else(|| Error::Protocol("no PowerState in CIM_ServiceAvailableToElement".into()))? {
            "2" => PowerState::On,
            "3" | "4" => PowerState::Sleep,
            "7" => PowerState::Hibernate,
            "6" | "8" | "13" => PowerState::Off,
            _ => PowerState::Unknown,
        })
    }

    pub async fn power(&mut self, action: PowerAction) -> Result<()> {
        // NOTE: 8 is a hard power off, go-wsman-messages calls it PowerOffHard and the handover had it wrong as "soft".
        let state = match action {
            PowerAction::On => 2,
            PowerAction::Off => 8,
            PowerAction::Cycle => 5,
            PowerAction::Reset => 10,
        };
        let input = request_power_state_change(state);
        self.invoke("CIM_PowerManagementService", "RequestPowerStateChange", &input).await?;
        Ok(())
    }
}

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
}
