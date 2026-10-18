//! A redirection session as recorded after its handshake: who said what, in order. On disk each record is a direction
//! byte (`>` we sent, `<` the device sent), a little-endian u32 length and the bytes.

use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Transcript(pub Vec<(Direction, Vec<u8>)>);

impl Transcript {
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut rest = bytes.as_slice();
        let mut records = Vec::new();
        while let [direction, a, b, c, d, tail @ ..] = rest {
            let len = u32::from_le_bytes([*a, *b, *c, *d]) as usize;
            let direction = if *direction == b'>' { Direction::Sent } else { Direction::Received };
            let data =
                tail.get(..len).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated transcript"))?;
            records.push((direction, data.to_vec()));
            rest = &tail[len..];
        }
        Ok(Self(records))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut bytes = Vec::new();
        for (direction, data) in &self.0 {
            bytes.push(if *direction == Direction::Sent { b'>' } else { b'<' });
            bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(data);
        }
        std::fs::write(path, bytes)
    }
}
