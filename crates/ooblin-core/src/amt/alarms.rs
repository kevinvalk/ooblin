use instant_xml::ToXml;
use jiff::{SpanRelativeTo, Timestamp, Unit};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::Client;
use super::wsman::method;
use crate::{Error, Result};

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Alarm {
    pub name: String,
    /// When it goes off next, in Unix seconds.
    pub next_at: i64,
    /// How long until it goes off again, 0 for an alarm that goes off once.
    pub every_minutes: u32,
    /// Whether AMT forgets it once it went off for the last time.
    pub is_forget: bool,
}

impl Client {
    pub async fn alarms(&mut self) -> Result<Vec<Alarm>> {
        let alarms = self.enumerate("IPS_AlarmClockOccurrence").await?;

        Ok(alarms
            .iter()
            .map(|alarm| Alarm {
                name: alarm.text("ElementName"),
                next_at: alarm.parse::<Timestamp>("StartTime").map_or(0, |start_time| start_time.as_second()),
                // AMT writes the interval as an ISO 8601 duration, `P1DT2H3M`.
                every_minutes: alarm.parse::<jiff::Span>("Interval").map_or(0, |interval| {
                    interval.total((Unit::Minute, SpanRelativeTo::days_are_24_hours())).unwrap_or(0.0) as u32
                }),
                is_forget: alarm.get("DeleteOnCompletion") == Some("true"),
            })
            .collect())
    }

    /// NOTE: AMT refuses an alarm in the past.
    pub(super) async fn add_alarm(&mut self, alarm: &Alarm) -> Result<()> {
        let start_time = Timestamp::from_second(alarm.next_at).map_err(|error| Error::Protocol(error.to_string()))?;
        let minutes = alarm.every_minutes;

        // Like MeshCommander, we use an alarm's name as its ID too.
        let template = AlarmTemplate {
            InstanceID: &alarm.name,
            ElementName: &alarm.name,
            StartTime: StartTime { Datetime: &start_time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string() },
            Interval: Interval { Interval: &format!("P{}DT{}H{}M", minutes / 1440, minutes / 60 % 24, minutes % 60) },
            DeleteOnCompletion: alarm.is_forget,
        };

        self.invoke(&AddAlarm { AlarmTemplate: template }).await?;
        Ok(())
    }

    pub(super) async fn delete_alarm(&mut self, name: &str) -> Result<()> {
        self.delete("IPS_AlarmClockOccurrence", name).await
    }
}

// The inputs below are from go-wsman-messages' `amt/alarmclock`, Copyright Intel Corporation, Apache-2.0.

const ALARM_CLOCK: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_AlarmClockService";
const OCCURRENCE: &str = "http://intel.com/wbem/wscim/1/ips-schema/1/IPS_AlarmClockOccurrence";
const COMMON: &str = "http://schemas.dmtf.org/wbem/wscim/1/common";

#[derive(ToXml)]
#[xml(rename = "AddAlarm_INPUT", ns(ALARM_CLOCK))]
struct AddAlarm<'a> {
    AlarmTemplate: AlarmTemplate<'a>,
}

method!(AddAlarm<'_>, ALARM_CLOCK);

// NOTE: This is an `IPS_AlarmClockOccurrence`, but in the alarm clock's namespace.
#[derive(ToXml)]
#[xml(ns(ALARM_CLOCK))]
struct AlarmTemplate<'a> {
    #[xml(ns(OCCURRENCE))]
    InstanceID: &'a str,
    #[xml(ns(OCCURRENCE))]
    ElementName: &'a str,
    StartTime: StartTime<'a>,
    Interval: Interval<'a>,
    #[xml(ns(OCCURRENCE))]
    DeleteOnCompletion: bool,
}

#[derive(ToXml)]
#[xml(ns(OCCURRENCE))]
struct StartTime<'a> {
    #[xml(ns(COMMON))]
    Datetime: &'a str,
}

#[derive(ToXml)]
#[xml(ns(OCCURRENCE))]
struct Interval<'a> {
    #[xml(ns(COMMON))]
    Interval: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amt::wsman::{self, body_meaning, meaning};

    #[test]
    fn adds_an_alarm_like_go_wsman_messages() {
        // From go-wsman-messages' `alarmclock/service_test.go`.
        let expected = r#"<r:AddAlarm_INPUT xmlns:r="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_AlarmClockService"><d:AlarmTemplate xmlns:d="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_AlarmClockService" xmlns:s="http://intel.com/wbem/wscim/1/ips-schema/1/IPS_AlarmClockOccurrence"><s:InstanceID>Instance</s:InstanceID><s:ElementName>Alarm instance name</s:ElementName><s:StartTime><p:Datetime xmlns:p="http://schemas.dmtf.org/wbem/wscim/1/common">2022-12-31T23:59:00Z</p:Datetime></s:StartTime><s:Interval><p:Interval xmlns:p="http://schemas.dmtf.org/wbem/wscim/1/common">P1DT23H59M</p:Interval></s:Interval><s:DeleteOnCompletion>true</s:DeleteOnCompletion></d:AlarmTemplate></r:AddAlarm_INPUT>"#;
        let template = AlarmTemplate {
            InstanceID: "Instance",
            ElementName: "Alarm instance name",
            StartTime: StartTime { Datetime: "2022-12-31T23:59:00Z" },
            Interval: Interval { Interval: "P1DT23H59M" },
            DeleteOnCompletion: true,
        };
        let request = wsman::invoke(&AddAlarm { AlarmTemplate: template }, None, 0);
        assert_eq!(body_meaning(&request), meaning(expected));
    }
}
