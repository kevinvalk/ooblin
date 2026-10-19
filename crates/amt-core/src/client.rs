use crate::net::Connection;
use instant_xml::ToXml;

use crate::wsman::{self, Instance, Method};
use crate::{Error, Result, Target};

pub struct Client {
    connection: Connection,
    message_id: u32,
}

impl Client {
    /// Connects lazily, the first call does the TLS handshake and checks the certificate against `pin`.
    pub fn new(target: &Target, pin: Option<String>) -> Result<Self> {
        Ok(Self { connection: Connection::new(target, pin)?, message_id: 0 })
    }

    /// Known after the first successful call.
    pub(crate) fn fingerprint(&self) -> Option<String> {
        self.connection.fingerprint()
    }

    pub async fn get(&mut self, class: &str) -> Result<Instance> {
        let envelope = wsman::get(class, self.next_message_id());
        wsman::parse_instance(&self.read(envelope).await?)
    }

    pub async fn put(&mut self, uri: &str, instance: &impl ToXml) -> Result<()> {
        let envelope = wsman::put(uri, instance, self.next_message_id());
        self.connection.post(envelope).await?;
        Ok(())
    }

    /// Calls a method that changes something, once. A non-zero `ReturnValue` is an error.
    pub async fn invoke<T: Method>(&mut self, input: &T) -> Result<Instance> {
        let envelope = wsman::invoke(input, self.next_message_id());
        output(T::NAME, &self.connection.post(envelope).await?)
    }

    /// Calls a method that only reads, like `invoke` but tried twice if need be (see `read`).
    pub async fn query<T: Method>(&mut self, input: &T) -> Result<Instance> {
        let envelope = wsman::invoke(input, self.next_message_id());
        output(T::NAME, &self.read(envelope).await?)
    }

    pub async fn enumerate(&mut self, class: &str) -> Result<Vec<Instance>> {
        let envelope = wsman::enumerate(class, self.next_message_id());
        // A class this device doesn't have (older firmware, say) simply has no instances.
        let mut context = match self.read(envelope).await {
            Err(Error::Fault(_)) => return Ok(Vec::new()),
            reply => Some(wsman::parse_enumerate(&reply?)?),
        };
        let mut instances = Vec::new();
        while let Some(current) = context {
            let envelope = wsman::pull(class, &current, self.next_message_id());
            let page = wsman::parse_pull(&self.read(envelope).await?)?;
            instances.extend(page.items);
            context = page.next;
        }
        Ok(instances)
    }

    /// Reading changes nothing, so a read lost on the way (AMT sometimes garbles a reply) gets a second try. The broken
    /// connection is gone by then, so it goes over a fresh one. A device that timed out gets none, it would only hang again.
    async fn read(&mut self, envelope: String) -> Result<String> {
        match self.connection.post(envelope.clone()).await {
            Err(Error::Transport(_)) => self.connection.post(envelope).await,
            reply => reply,
        }
    }

    fn next_message_id(&mut self) -> u32 {
        self.message_id += 1;
        self.message_id
    }
}

fn output(method: &str, reply: &str) -> Result<Instance> {
    let output = wsman::parse_instance(reply)?;
    match output.get("ReturnValue") {
        Some("0") | None => Ok(output),
        Some(code) => Err(Error::Method { method: method.to_owned(), code: code.to_owned() }),
    }
}
