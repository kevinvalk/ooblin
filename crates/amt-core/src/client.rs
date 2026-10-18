use serde::Serialize;

use crate::net::{Connection, Fingerprint};
use crate::wsman::{self, Instance};
use crate::{Result, Target};

pub struct Client {
    connection: Connection,
    trust: Trust,
    message_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Trust {
    /// The certificate matched the pin we were given.
    Pinned,
    /// We had no pin, so we trusted the first certificate we saw.
    FirstUse,
}

impl Client {
    /// Connects lazily, the first call does the TLS handshake and pin check.
    pub fn new(target: &Target, pin: Option<Fingerprint>) -> Result<Self> {
        let trust = if pin.is_some() { Trust::Pinned } else { Trust::FirstUse };
        Ok(Self {
            connection: Connection::new(target, pin)?,
            trust,
            message_id: 0,
        })
    }

    pub fn trust(&self) -> Trust {
        self.trust
    }

    /// Known after the first successful call.
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        self.connection.fingerprint()
    }

    pub async fn get(&mut self, class: &str) -> Result<Instance> {
        let envelope = wsman::get(class, self.next_message_id());
        wsman::parse_get(&self.connection.post(envelope).await?)
    }

    pub async fn enumerate(&mut self, class: &str) -> Result<Vec<Instance>> {
        let envelope = wsman::enumerate(class, self.next_message_id());
        let mut context = Some(wsman::parse_enumerate(&self.connection.post(envelope).await?)?);
        let mut instances = Vec::new();
        while let Some(current) = context {
            let envelope = wsman::pull(class, &current, self.next_message_id());
            let page = wsman::parse_pull(&self.connection.post(envelope).await?)?;
            instances.extend(page.items);
            context = page.next;
        }
        Ok(instances)
    }

    fn next_message_id(&mut self) -> u32 {
        self.message_id += 1;
        self.message_id
    }
}
