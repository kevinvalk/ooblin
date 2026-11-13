//! Virtual media: a USB CD-ROM drive on the host whose disc is an image file here, over the BMC's `/vm` WebSocket as
//! its HTML5 viewer does it (`vstorage.js`), and ikvmt (Apache-2.0) after it. The BMC hands us the host's commands in
//! USB mass storage's wrappers, `Cdrom` answers them.

use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Client, Web};
use crate::cdrom::{Answer, Cdrom};
use crate::net::ikvm::{self, Plug, Receiver};
use crate::net::redfish::Connection;
use crate::{Error, GOODBYE, Result};

/// A disc in the host's drive, served by `serve`. The web session it went in with ends with it.
pub struct Media {
    receiver: Receiver,
    plug: Plug,
    cdrom: Cdrom,
    web: Web,
}

// The PDUs' classes.
const PLUG_IN: [u8; 4] = [0, 0, 0, 1];
const PLUGGED_IN: [u8; 4] = [0, 0, 0, 2];
const ALIVE: [u8; 4] = [0, 0, 0, 3];
const KEEP_ALIVE: [u8; 4] = [0, 0, 0, 4];
const PLUG_OUT: [u8; 4] = [0, 0, 0, 5];
const PLUGGED_OUT: [u8; 4] = [0, 0, 0, 6];
const ENDPOINTS: [u8; 4] = [0, 0, 0, 7];
const SLOTS: [u8; 4] = [0, 0, 0, 8];

/// The most a PDU carries, as ikvmt.
const MOST: usize = 1 << 20;

impl Client {
    /// Plugs a USB CD-ROM drive with `image` in it into the host, in the first of the BMC's three slots that's free.
    pub async fn media(&mut self, image: std::fs::File) -> Result<Media> {
        let cdrom = Cdrom::new(image).await?;
        let web = Web::login(&self.connection).await?;
        match plug_in(&self.connection, &web).await {
            Ok((receiver, plug)) => Ok(Media { receiver, plug, cdrom, web }),
            Err(error) => {
                web.logout().await;
                Err(error)
            }
        }
    }
}

/// Into the first free slot, as the viewer: the slots asked on a socket of their own, the KVM token for a password,
/// then the drive's endpoints.
async fn plug_in(connection: &Connection, web: &Web) -> Result<(Receiver, Plug)> {
    let token = web.token().await?;
    let (mut receiver, mut plug) = ikvm::media(connection, web).await?;
    plug.send(&pdu(SLOTS, &[])).await?;
    let slots = next(&mut receiver, &mut plug, false).await;
    plug.close().await;
    let (_, slots) = slots?;
    let Some(slot) = slots.get(1..4).and_then(|slots| slots.iter().position(|&slot| slot == 255)) else {
        return Err(Error::Invalid("All three of the BMC's virtual drives are in use.".into()));
    };

    let (mut receiver, mut plug) = ikvm::media(connection, web).await?;
    plug.send(&plug_in_packet(&token, slot as u8)?).await?;
    let (class, status) = next(&mut receiver, &mut plug, false).await?;
    let why = match (class, status.first()) {
        (PLUGGED_IN, Some(0)) => {
            plug.send(&pdu(ENDPOINTS, &[6, 3, 1, 0x10, 2, 0x20, 3, 0x30])).await?;
            return Ok((receiver, plug));
        }
        (PLUGGED_IN, Some(1)) => "refused the KVM token",
        (PLUGGED_IN, Some(2)) => "is busy",
        (PLUGGED_IN, Some(3)) => "isn't for this user",
        (PLUGGED_IN, Some(5)) => "is off while the BMC's firmware updates",
        (PLUGGED_IN, Some(6)) => "says the web session expired",
        _ => "said no",
    };
    plug.close().await;
    Err(Error::Protocol(format!("the BMC's virtual media {why}: {class:?} {status:?}")))
}

impl Media {
    /// Answers the host until the BMC takes the drive out, or `eject` does, telling `read` what of the image, in bytes,
    /// after every read.
    pub async fn serve(mut self, mut read: impl FnMut(Range<u64>), eject: impl Future<Output = ()>) -> Result<()> {
        let served = tokio::select! {
            served = self.answer(&mut read) => served,
            () = eject => self.plug_out().await,
        };
        self.plug.close().await;
        self.web.logout().await;
        served
    }

    async fn answer(&mut self, read: &mut impl FnMut(Range<u64>)) -> Result<()> {
        loop {
            let (class, wrapper) = next(&mut self.receiver, &mut self.plug, true).await?;
            if class == PLUGGED_OUT {
                return Ok(());
            }
            // USB mass storage's command wrapper, for drive `class[2]`.
            if wrapper.len() != 31 || !wrapper.starts_with(b"USBC") || !(1..=16).contains(&wrapper[14]) {
                return Err(Error::Protocol(format!("a virtual media message we don't know: {class:?} {wrapper:?}")));
            }
            let length = u32::from_le_bytes(wrapper[8..12].try_into().unwrap()) as usize;
            let to_host = wrapper[12] & 0x80 != 0;
            // What the host writes comes next, a CD-ROM has no use for it.
            if !to_host && length > 0 {
                next(&mut self.receiver, &mut self.plug, false).await?;
            }
            let (mut data, range, failed) = match self.cdrom.answer(&wrapper[15..15 + usize::from(wrapper[14])]) {
                Answer::Data(data) => (data, 0..0, false),
                Answer::Read { lba, blocks } => {
                    let start = lba * 2048;
                    let range = start..(start + blocks * 2048).min(start + length.min(MOST) as u64);
                    let mut data = vec![0; (range.end - start) as usize];
                    self.cdrom.read(start, &mut data).await?;
                    (data, range, false)
                }
                Answer::Done => (vec![], 0..0, false),
                Answer::Failed(_) => (vec![], 0..0, true),
            };
            data.truncate(length);
            let residue = if to_host || failed { length - data.len() } else { 0 };
            let status = [b"USBS", &wrapper[4..8], &(residue as u32).to_le_bytes()[..], &[u8::from(failed)]].concat();
            let device = class[2];
            let mut answer = if to_host { pdu([0x22, 0, device, 0], &data) } else { vec![] };
            answer.extend(pdu([0x22, 0, device, 0xFF], &status));
            self.plug.send(&answer).await?;
            if !range.is_empty() {
                read(range);
            }
        }
    }

    /// Takes the drive out, waiting `GOODBYE` at most for the BMC to say it's out.
    async fn plug_out(&mut self) -> Result<()> {
        self.plug.send(&pdu(PLUG_OUT, &[])).await?;
        let out = async {
            while next(&mut self.receiver, &mut self.plug, true).await?.0 != PLUGGED_OUT {}
            Ok(())
        };
        tokio::time::timeout(GOODBYE, out).await.unwrap_or(Ok(()))
    }
}

/// The BMC's next PDU, its class and body, keep-alives answered on the way. Its start is `wait`ed for as long as it
/// takes, or as long as anything on the BMC's web port.
async fn next(receiver: &mut Receiver, plug: &mut Plug, wait: bool) -> Result<([u8; 4], Vec<u8>)> {
    loop {
        let header = match receiver.read(8).await {
            Err(Error::Timeout) if wait => continue,
            header => header?,
        };
        let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        if length > MOST {
            return Err(Error::Protocol(format!("a {length}-byte virtual media message")));
        }
        let (class, body) = (header[..4].try_into().unwrap(), receiver.read(length).await?);
        if class != KEEP_ALIVE {
            return Ok((class, body));
        }
        plug.send(&pdu(ALIVE, &[0xFF; 4])).await?;
    }
}

fn pdu(class: [u8; 4], body: &[u8]) -> Vec<u8> {
    [&class[..], &(body.len() as u32).to_le_bytes(), body].concat()
}

/// As the viewer: the KVM token split at 15 bytes, a random stamp, one drive in `slot` of an authenticated HTML5
/// viewer's, a CD-ROM. Then the drive's USB descriptors, which the PDU's length leaves out.
fn plug_in_packet(token: &str, slot: u8) -> Result<Vec<u8>> {
    let token = token.as_bytes();
    if !(15..=35).contains(&token.len()) {
        return Err(Error::Protocol(format!("a {}-byte KVM token", token.len())));
    }
    let mut packet = pdu(PLUG_IN, &[0; 44]);
    packet[8..23].copy_from_slice(&token[..15]);
    packet[24..24 + token.len() - 15].copy_from_slice(&token[15..]);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos() & 0x7FFF;
    packet[44..48].copy_from_slice(&stamp.to_be_bytes());
    packet[48] = 0xC1 | (slot + 1) << 1;
    packet[49] = 3;
    for descriptor in DESCRIPTORS {
        packet.push(descriptor.len() as u8);
        packet.extend(descriptor);
    }
    Ok(packet)
}

/// The viewer's drive's, as ikvmt has them: the device (ATEN's), its configuration (one mass storage interface, SCSI
/// over bulk-only transport), the language, its strings ("Flash Disk", a serial number and another), a class reply and
/// the device qualifier.
const DESCRIPTORS: [&[u8]; 8] = [
    &[18, 1, 0, 2, 0, 0, 0, 64, 0x1F, 0x0B, 0xEA, 3, 0, 2, 0, 0, 0, 1],
    &[
        9, 2, 39, 0, 1, 1, 0, 128, 100, 9, 4, 0, 0, 3, 8, 6, 80, 0, 7, 5, 1, 2, 0, 2, 255, 7, 5, 130, 2, 0, 2, 255, 7,
        5, 131, 3, 2, 0, 1,
    ],
    &[4, 3, 9, 4],
    &[
        34, 3, 70, 0, 108, 0, 97, 0, 115, 0, 104, 0, 32, 0, 68, 0, 105, 0, 115, 0, 107, 0, 32, 0, 32, 0, 32, 0, 32, 0,
        32, 0, 32, 0,
    ],
    &[
        34, 3, 52, 0, 69, 0, 56, 0, 70, 0, 48, 0, 57, 0, 50, 0, 67, 0, 51, 0, 70, 0, 68, 0, 55, 0, 70, 0, 56, 0, 70, 0,
        55, 0,
    ],
    &[26, 3, 83, 0, 78, 0, 48, 0, 48, 0, 48, 0, 80, 0, 81, 0, 73, 0, 48, 0, 48, 0, 57, 0, 32, 0],
    &[0],
    &[10, 6, 0, 2, 0, 0, 0, 64, 1, 0],
];
