use std::collections::HashMap;
use std::time::{Duration, Instant};

use flate2::{Decompress, FlushDecompress};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::Mutex;
use ts_rs::TS;

use super::Client;
use crate::net::{Redirection, RedirectionSession, Stream, read_exact, write_all};
use crate::{Error, GOODBYE, Input, Result};

// NOTE: KVM is the device's screen, keyboard and mouse, over RFB on a redirection session. We follow MeshCommander's
// `amt-desktop-0.0.2.js` and Intel's ui-toolkit (both Apache-2.0), and RFC 6143 where they leave off. We decode in Rust
// so the webview only paints.

/// An open KVM session. The screen and what we send go separate ways, so typing never waits for the screen.
pub struct Kvm {
    screen: Mutex<Screen>,
    keyboard: Mutex<Keyboard>,
}

struct Screen {
    reader: ReadHalf<Stream>,
    inflater: Decompress, // ZRLE is one zlib stream for the whole session.
    width: u16,
    height: u16,
    bytes_per_pixel: usize, // 2 (RGB565), or 1 (RGB332) for a screen too large for AMT at 2

    has_asked: bool, // Did we ask for an update the device has not sent yet?
    is_whole: bool,  // Is the next update we ask for one of the whole screen?

    // NOTE: AMT misses some changes (a screen gone black where text was, say), which asking for the whole screen anew
    // puts right. We ask for it `QUIET` after the last change, or `BUSY` after we last did while changes keep coming.
    changed_at: Option<Instant>, // When the last change came since we last asked for the whole screen
    anew_at: Instant,            // When we last asked for the whole screen
    is_answer_due: bool,         // The answer to that is still to come, and we do not count it as a change.
}

const QUIET: Duration = Duration::from_millis(500);
const BUSY: Duration = Duration::from_secs(1);

// The keyboard also sends our update requests, as everything we write goes through the one writer.
struct Keyboard {
    writer: WriteHalf<Stream>,
    held: HashMap<String, u32>, // The keysym each held key went down as
    pointer: [u8; 6],           // The mouse as we last sent it
}

// 16-bit RGB565, little-endian. This is AMT's own pixel format, which MeshCommander and Intel's console keep.
const RGB565: [u8; 13] = [16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0];

// 8-bit RGB332, as MeshCommander and Intel's console ask for it. We need it for a screen whose RGB565 would be more
// than AMT keeps, which happens beyond 4K and from AMT 18 on.
const RGB332: [u8; 13] = [8, 8, 0, 1, 0, 7, 0, 7, 0, 3, 5, 2, 0];
const RGB565_MAX_BYTES: usize = 9_216_000; // MeshCommander's limit

// No screen comes close to this (8K whole is 133 MB of RGBA), so an update of rectangle after rectangle cannot fill our
// memory.
const FRAME_MAX_BYTES: usize = 1 << 28;

/// The device's screens KVM can show, by number from 0, and the one it shows. Like MeshCommander, there is a choice
/// once more than one is active.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Screens {
    pub active: Vec<u8>,
    pub shown: u8,
}

impl Client {
    pub async fn kvm(&mut self) -> Result<Kvm> {
        Kvm::open(self.redirect(RedirectionSession::Kvm).await?).await
    }

    pub async fn screens(&mut self) -> Result<Screens> {
        let settings = self.get("IPS_KVMRedirectionSettingData").await?;
        let screens = self.enumerate("IPS_ScreenSettingData").await?;
        let active = screens.first().into_iter().flat_map(|screens| screens.all("IsActive"));
        let active = active.zip(0..=255).filter(|(active, _)| *active == "true").map(|(_, screen)| screen).collect();

        Ok(Screens { active, shown: settings.parse("DefaultScreen").unwrap_or_default() })
    }

    /// Shows `screen` in KVM, also in a session that is already open.
    pub async fn show_screen(&mut self, screen: u8) -> Result<()> {
        let mut settings = self.get("IPS_KVMRedirectionSettingData").await?;
        settings.set("DefaultScreen", screen);
        self.put("IPS_KVMRedirectionSettingData", &settings).await
    }
}

impl Kvm {
    async fn open(mut redirection: Redirection) -> Result<Self> {
        // AMT starts the KVM data and then RFB, where AMT 19 says 4.0 and takes 3.8.
        redirection.write_all(&[0x40, 0, 0, 0, 0, 0, 0, 0]).await?;
        redirection.read_exact(8 + 12).await?;
        redirection.write_all(b"RFB 003.008\n").await?;

        // We pick no RFB security, as the redirection handshake already authenticated us.
        let security_types = redirection.read_exact(1).await?[0];
        redirection.read_exact(security_types.into()).await?;
        redirection.write_all(&[1]).await?;
        if redirection.read_exact(4).await? != [0; 4] {
            return Err(Error::Protocol("the device refused the KVM session".into()));
        }

        // We ask for a shared screen.
        redirection.write_all(&[1]).await?;
        let server_init = redirection.read_exact(24).await?;
        let name_length = u32::from_be_bytes(server_init[20..24].try_into().unwrap()) as usize;
        redirection.read_exact(name_length).await?;
        if server_init[4..17] != RGB565 {
            return Err(Error::Protocol(format!("a pixel format we don't decode: {:?}", &server_init[4..17])));
        }

        // We take ZRLE, Raw (AMT insists on it) and the DesktopSize pseudo-encoding.
        redirection.write_all(&[2, 0, 0, 3, 0, 0, 0, 16, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x21]).await?;

        let (reader, writer) = redirection.split();
        let width = u16::from_be_bytes([server_init[0], server_init[1]]);
        let height = u16::from_be_bytes([server_init[2], server_init[3]]);
        let inflater = Decompress::new(true);
        let screen = Screen {
            reader,
            inflater,
            width,
            height,
            bytes_per_pixel: 2,
            has_asked: false,
            is_whole: true,
            changed_at: None,
            anew_at: Instant::now(),
            is_answer_due: false,
        };
        let keyboard = Keyboard { writer, held: HashMap::new(), pointer: [5, 0, 0, 0, 0, 0] };

        Ok(Self { screen: Mutex::new(screen), keyboard: Mutex::new(keyboard) })
    }

    /// The next change on screen, the way a canvas takes it: the screen's width and height, then per changed
    /// rectangle its x, y, width and height and its pixels as RGBA, numbers as little-endian u16s. It is just the size
    /// if nothing changed for a second, so a screen nobody asks for any more lets go of its session soon.
    pub async fn frame(&self) -> Result<Vec<u8>> {
        let mut screen = self.screen.lock().await;
        self.ask(&mut screen).await?;

        let mut frame = vec![0; 4];
        loop {
            // We wait until the whole screen anew is due, or a second. We read a single byte here because that loses
            // nothing when it times out.
            let due_at = screen.changed_at.map(|changed_at| (changed_at + QUIET).min(screen.anew_at + BUSY));
            let wait = due_at.map_or(BUSY, |due_at| due_at.saturating_duration_since(Instant::now()).min(BUSY));
            let kind = match tokio::time::timeout(wait, screen.reader.read_u8()).await {
                Err(_) => {
                    if due_at.is_some_and(|due_at| due_at <= Instant::now()) {
                        self.request(&mut screen, true).await?;
                    }
                    return Ok(screen.size(frame));
                }
                Ok(kind) => kind?,
            };

            match kind {
                0 => break,
                2 => {} // The Bell, which AMT 19 rings before its first update
                // SetColourMapEntries, which a true colour screen never needs.
                1 => {
                    let header = screen.read(5).await?;
                    screen.read(usize::from(u16::from_be_bytes([header[3], header[4]])) * 6).await?;
                }
                // ServerCutText, which we have no use for.
                3 => {
                    let header = screen.read(7).await?;
                    screen.read(u32::from_be_bytes(header[3..7].try_into().unwrap()) as usize).await?;
                }
                other => return Err(Error::Protocol(format!("an RFB message we don't know: {other}"))),
            }
        }

        let header = screen.read(3).await?;
        for _ in 0..u16::from_be_bytes([header[1], header[2]]) {
            let rectangle = screen.read(12).await?;
            let [x, y, width, height] =
                [0, 2, 4, 6].map(|offset| u16::from_be_bytes([rectangle[offset], rectangle[offset + 1]]));

            match i32::from_be_bytes(rectangle[8..12].try_into().unwrap()) {
                // DesktopSize: the screen changed size, so the next update we ask for covers all of it.
                -223 => (screen.width, screen.height, screen.is_whole) = (width, height, true),
                // Raw and ZRLE: AMT sends at most 64 by 64 at a time, a tile.
                encoding @ (0 | 16) if (1..=64).contains(&width) && (1..=64).contains(&height) => {
                    frame.extend([x, y, width, height].iter().flat_map(|value| value.to_le_bytes()));
                    let (pixels, bytes_per_pixel) = (usize::from(width) * usize::from(height), screen.bytes_per_pixel);

                    if encoding == 0 {
                        let raw = screen.read(pixels * bytes_per_pixel).await?;
                        frame.extend(raw.chunks(bytes_per_pixel).flat_map(rgba));
                    } else {
                        let length = u32::from_be_bytes(screen.read(4).await?.try_into().unwrap());
                        let data = screen.read(length as usize).await?;

                        // NOTE: A tile AMT did not compress comes as a zlib stored block (0, then its length) outside
                        // the stream. Like Intel's console and MeshCommander we take it as it is, because it must NOT
                        // go into the inflater's history.
                        let is_stored = data.len() > 5
                            && data[0] == 0
                            && usize::from(u16::from_le_bytes([data[1], data[2]])) == data.len() - 5;
                        let tile = if is_stored { data[5..].to_vec() } else { screen.inflate(&data)? };

                        zrle(&tile, bytes_per_pixel, width.into(), height.into(), &mut frame)
                            .ok_or_else(|| Error::Protocol("a ZRLE tile we can't decode".into()))?;
                    }
                }
                encoding => return Err(Error::Protocol(format!("a {width} by {height} rectangle in {encoding}"))),
            }

            if frame.len() > FRAME_MAX_BYTES {
                return Err(Error::Protocol(format!("an update of more than {FRAME_MAX_BYTES} bytes")));
            }
        }

        if frame.len() > 4 && !std::mem::take(&mut screen.is_answer_due) {
            screen.changed_at = Some(Instant::now());
        }

        // We ask right away, so the next update is on its way while this one is painted.
        screen.has_asked = false;
        self.ask(&mut screen).await?;

        Ok(screen.size(frame))
    }

    /// Asks for the next update unless we already did, in the pixel format the screen's size wants. Changing that
    /// format means we need the whole screen anew.
    async fn ask(&self, screen: &mut Screen) -> Result<()> {
        if screen.has_asked {
            return Ok(());
        }

        let pixels = usize::from(screen.width) * usize::from(screen.height);
        let bytes_per_pixel = if pixels * 2 > RGB565_MAX_BYTES { 1 } else { 2 };
        if bytes_per_pixel != screen.bytes_per_pixel {
            let format = if bytes_per_pixel == 1 { RGB332 } else { RGB565 };
            self.keyboard.lock().await.send(&[&[0, 0, 0, 0][..], &format, &[0; 3]].concat()).await?;
            (screen.bytes_per_pixel, screen.is_whole) = (bytes_per_pixel, true);
        }

        let is_whole = screen.is_whole || screen.changed_at.is_some() && screen.anew_at.elapsed() >= BUSY;
        self.request(screen, is_whole).await?;
        (screen.has_asked, screen.is_whole) = (true, false);

        Ok(())
    }

    /// Asks for an update of what changed, or of the whole screen.
    async fn request(&self, screen: &mut Screen, is_whole: bool) -> Result<()> {
        let request =
            [[3, u8::from(!is_whole)], [0; 2], [0; 2], screen.width.to_be_bytes(), screen.height.to_be_bytes()];
        self.keyboard.lock().await.send(&request.concat()).await?;

        if is_whole {
            (screen.changed_at, screen.anew_at, screen.is_answer_due) = (None, Instant::now(), true);
        }

        Ok(())
    }

    /// We wait for a frame that is still on its way (a second at most), let go of what is held and say goodbye
    /// instead of just going, because AMT takes one KVM session at a time and the next one may already be coming. A
    /// device gone silent mid-frame gets no goodbye, see `GOODBYE`.
    pub async fn close(&self) {
        let goodbye = async {
            let _screen = self.screen.lock().await;
            let mut keyboard = self.keyboard.lock().await;
            keyboard.input(Input::Release).await?;
            Ok::<_, Error>(keyboard.writer.shutdown().await?)
        };
        let _ = tokio::time::timeout(GOODBYE, goodbye).await;
    }

    /// We take the keyboard for one input at a time, and for text one character at a time. Typing text takes a while,
    /// in which the screen must be able to ask for updates, other input to come and `close` to say goodbye.
    pub async fn input(&self, inputs: Vec<Input>) -> Result<()> {
        let singly = inputs.into_iter().flat_map(|input| match input {
            Input::Text { text } => text.chars().map(|char| Input::Text { text: char.into() }).collect(),
            input => vec![input],
        });

        for input in singly {
            self.keyboard.lock().await.input(input).await?;
        }

        Ok(())
    }
}

impl Screen {
    async fn read(&mut self, length: usize) -> Result<Vec<u8>> {
        read_exact(&mut self.reader, length).await
    }

    /// Inflates one tile's worth of the zlib stream.
    fn inflate(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        // A 64 by 64 tile inflates to at most 3 bytes a pixel (RLE in RGB565, runs of one) and its palette. Anything
        // more is not a tile, so we do not give the inflater more room than that.
        let mut tile = Vec::with_capacity(64 * 64 * 3 + 512);

        let before = self.inflater.total_in();
        let inflated = self.inflater.decompress_vec(data, &mut tile, FlushDecompress::Sync);
        if inflated.is_err() || self.inflater.total_in() - before != data.len() as u64 {
            return Err(Error::Protocol("ZRLE data that doesn't inflate to a tile".into()));
        }

        Ok(tile)
    }

    fn size(&self, mut frame: Vec<u8>) -> Vec<u8> {
        frame[..4].copy_from_slice(&[self.width.to_le_bytes(), self.height.to_le_bytes()].concat());
        frame
    }
}

impl Keyboard {
    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        write_all(&mut self.writer, bytes).await
    }

    async fn input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Key { code, is_down: true } => {
                let is_shifted = self.held.contains_key("ShiftLeft") || self.held.contains_key("ShiftRight");
                if let Some(keysym) = keysym(&code, is_shifted) {
                    self.held.insert(code, keysym);
                    self.send(&key(keysym, true)).await?;
                }
            }
            Input::Key { code, is_down: false } => {
                if let Some(keysym) = self.held.remove(&code) {
                    self.send(&key(keysym, false)).await?;
                }
            }
            Input::Pointer { x, y, buttons, scroll } => {
                // The DOM's buttons are left, right and middle, where RFB's are left, middle and right.
                let buttons = buttons & 1 | (buttons & 2) << 1 | (buttons & 4) >> 1;
                let (x, y) = (x.to_be_bytes(), y.to_be_bytes());

                // A notch of the wheel is a click of button 4 (up) or 5 (down).
                if scroll != 0 {
                    let notch = buttons | if scroll < 0 { 8 } else { 16 };
                    self.send(&[5, notch, x[0], x[1], y[0], y[1]]).await?;
                }

                let pointer = [5, buttons, x[0], x[1], y[0], y[1]];
                self.send(&pointer).await?;
                self.pointer = pointer;
            }
            Input::Text { text } => {
                // The Latin-1 keysyms of a US keyboard are the characters themselves, with Shift held for the shifted
                // ones. We type at a person's pace so the device keeps up.
                for char in text.chars() {
                    let (keysym, is_shifted) = match char {
                        '\n' => (RETURN, false),
                        '\t' => (TAB, false),
                        ' '..='~' => {
                            (u32::from(char), char.is_ascii_uppercase() || r#"~!@#$%^&*()_+{}|:"<>?"#.contains(char))
                        }
                        _ => continue,
                    };
                    let keys = [key(keysym, true), key(keysym, false)].concat();
                    let keys = if is_shifted { [key(SHIFT, true), keys, key(SHIFT, false)].concat() } else { keys };
                    self.send(&keys).await?;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            Input::Release => {
                for keysym in std::mem::take(&mut self.held).into_values() {
                    self.send(&key(keysym, false)).await?;
                }

                // The mouse stays where it was, with its buttons up.
                let mut released = self.pointer;
                released[1] = 0;
                if released != self.pointer {
                    self.send(&released).await?;
                    self.pointer = released;
                }
            }
        }

        Ok(())
    }
}

const TAB: u32 = 0xff09;
const RETURN: u32 = 0xff0d;
const SHIFT: u32 = 0xffe1;

fn key(keysym: u32, is_down: bool) -> Vec<u8> {
    [&[4, u8::from(is_down), 0, 0][..], &keysym.to_be_bytes()].concat()
}

/// The X11 keysym of a key by its DOM `code`, where letters follow Shift. The digits and the punctuation are the ones
/// MeshCommander and Intel's console send. For the rest we send X11's own where theirs are not keysyms (they send
/// Print Screen as a comma).
fn keysym(code: &str, is_shifted: bool) -> Option<u32> {
    let char = |prefix| match code.strip_prefix(prefix)?.as_bytes() {
        &[char] => Some(char),
        _ => None,
    };
    if let Some(letter) = char("Key") {
        return Some(u32::from(if is_shifted { letter } else { letter.to_ascii_lowercase() }));
    }
    if let Some(digit) = char("Digit").or_else(|| char("Numpad")) {
        return Some(u32::from(digit));
    }

    let function = code.strip_prefix('F').and_then(|number| number.parse::<u32>().ok());
    if let Some(number) = function.filter(|number| (1..=12).contains(number)) {
        return Some(0xffbe + number - 1);
    }

    Some(match code {
        "Space" => 0x20,
        "Quote" => 0x27,
        "NumpadMultiply" => 0x2a,
        "NumpadAdd" => 0x2b,
        "Comma" => 0x2c,
        "Minus" | "NumpadSubtract" => 0x2d,
        "Period" | "NumpadDecimal" => 0x2e,
        "Slash" | "NumpadDivide" => 0x2f,
        "Semicolon" => 0x3b,
        "Equal" => 0x3d,
        "BracketLeft" => 0x5b,
        "Backslash" => 0x5c,
        "BracketRight" => 0x5d,
        "Backquote" => 0x60,
        "Backspace" => 0xff08,
        "Tab" => TAB,
        "Enter" | "NumpadEnter" => RETURN,
        "Pause" => 0xff13,
        "ScrollLock" => 0xff14,
        "Escape" => 0xff1b,
        "Home" => 0xff50,
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "End" => 0xff57,
        "PrintScreen" => 0xff61,
        "Insert" => 0xff63,
        "ContextMenu" => 0xff67,
        "NumLock" => 0xff7f,
        "ShiftLeft" => SHIFT,
        "ShiftRight" => 0xffe2,
        "ControlLeft" => 0xffe3,
        "ControlRight" => 0xffe4,
        "CapsLock" => 0xffe5,
        "MetaLeft" => 0xffe7,
        "MetaRight" => 0xffe8,
        "AltLeft" => 0xffe9,
        "AltRight" => 0xffea,
        "Delete" => 0xffff,
        _ => return None,
    })
}

/// An RGB565 or RGB332 pixel as RGBA. We stretch each channel to the full byte so white stays white.
fn rgba(pixel: &[u8]) -> [u8; 4] {
    match *pixel {
        [low, high] => {
            let pixel = u16::from_le_bytes([low, high]);
            let (r, g, b) = ((pixel >> 11) as u8, (pixel >> 5 & 63) as u8, (pixel & 31) as u8);
            [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2, 255]
        }
        [byte] => RGB332_RGBA[usize::from(byte)],
        _ => unreachable!("A pixel of {} bytes, did we add a pixel format without decoding it?!", pixel.len()),
    }
}

// Every RGB332 pixel as RGBA, which we work out once at compile time.
const RGB332_RGBA: [[u8; 4]; 256] = {
    let mut table = [[0; 4]; 256];
    let mut pixel = 0;
    while pixel < 256 {
        let (r, g, b) = ((pixel >> 5) as u8, (pixel >> 2 & 7) as u8, (pixel & 3) as u8);
        table[pixel] = [r << 5 | r << 2 | r >> 1, g << 5 | g << 2 | g >> 1, b * 85, 255];
        pixel += 1;
    }
    table
};

/// Decodes a ZRLE tile (RFC 6143 7.7.6) to RGBA. `None` when the data does not add up to the tile.
fn zrle(mut data: &[u8], bytes_per_pixel: usize, width: usize, height: usize, out: &mut Vec<u8>) -> Option<()> {
    let data = &mut data;
    let pixel = |data: &mut &[u8]| Some(rgba(data.split_off(..bytes_per_pixel)?));
    let end = out.len() + width * height * 4;

    let kind = *data.split_off_first()?;
    let colors = match kind {
        2..=16 => kind,
        130.. => kind - 128,
        _ => 0,
    };
    let palette: Vec<_> = (0..colors).map(|_| pixel(data)).collect::<Option<_>>()?;

    // A run is one more than its bytes add up to, where a byte of 255 says another follows.
    let run = |data: &mut &[u8]| {
        let mut length = 1;
        loop {
            let byte = *data.split_off_first()?;
            length += usize::from(byte);
            if byte != 255 {
                return Some(length);
            }
        }
    };
    let fill = |out: &mut Vec<u8>, color: [u8; 4], length: usize| {
        (out.len() + length * 4 <= end).then(|| out.extend(color.repeat(length)))
    };

    match kind {
        0 => (0..width * height).try_for_each(|_| fill(out, pixel(data)?, 1))?,
        1 => fill(out, pixel(data)?, width * height)?,
        // NOTE: Indexes of 1, 2 or 4 bits, which we read packed across rows. That is how Intel's console and
        // MeshCommander read AMT's, where RFC 6143 starts each row on a byte!
        2..=16 => {
            let bits = match colors {
                2 => 1,
                3 | 4 => 2,
                _ => 4,
            };
            let indexes = data.split_off(..(width * height * bits).div_ceil(8))?;
            for i in 0..width * height {
                let index = indexes[i * bits / 8] >> (8 - bits - i * bits % 8) & ((1 << bits) - 1);
                fill(out, *palette.get(usize::from(index))?, 1)?;
            }
        }
        128 => {
            while out.len() < end {
                let color = pixel(data)?;
                fill(out, color, run(data)?)?;
            }
        }
        130.. => {
            while out.len() < end {
                let index = *data.split_off_first()?;
                let length = if index & 128 == 0 { 1 } else { run(data)? };
                fill(out, *palette.get(usize::from(index & 127))?, length)?;
            }
        }
        _ => return None,
    }

    (out.len() == end).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rgb332_like_meshcommander() {
        // MeshCommander's `SetPixelFormat` for it is `0, 0, 0, 0, 8, 8, 0, 1, 7, 7, 3 (as u16s), 5, 2, 0, 0, 0, 0`.
        let set_pixel_format = [0, 0, 0, 0, 8, 8, 0, 1, 0, 7, 0, 7, 0, 3, 5, 2, 0, 0, 0, 0];
        assert_eq!([&[0, 0, 0, 0][..], &RGB332, &[0; 3]].concat(), set_pixel_format);

        // Red is the top 3 bits, green the next 3 and blue the last 2, each stretched to the full byte.
        let colors = [0xE0, 0x1C, 0x03, 0xFF, 0].map(|pixel| rgba(&[pixel]));
        assert_eq!(colors, [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255; 4], [0, 0, 0, 255]]);

        // A ZRLE tile of one color, with a byte a pixel.
        let mut out = Vec::new();
        assert_eq!(zrle(&[1, 0xE0], 1, 2, 1, &mut out), Some(()));
        assert_eq!(out, [255, 0, 0, 255].repeat(2));
    }
}
