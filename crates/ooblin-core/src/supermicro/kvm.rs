//! The BMC's screen, keyboard and mouse over ATEN's iKVM: RFB with messages of its own, the screen in AST2100 video.
//! We follow the BMC's own HTML5 viewer.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use super::ast2100::Decoder;
use super::{Client, Ikvm, Web};
use crate::net::ikvm::{Receiver, Sender};
use crate::{Error, GOODBYE, Input, Result};

/// An open KVM session, the same to the app as AMT's: the screen and what we send go separate ways, so typing never
/// waits for the screen. The web session it opened in ends with it.
pub struct Kvm {
    screen: Mutex<Screen>,
    keyboard: Mutex<Keyboard>,
    web: Web,
}

struct Screen {
    receiver: Receiver,
    /// The picture so far, `width` by `height`.
    decoder: Decoder,
    width: u16,
    height: u16,
    /// When we ask again for an update that hasn't come, a second after we asked, and the next a second after the BMC
    /// said it has no picture (the host's screen is off, say), as the viewer does. Whether it's for the whole screen.
    ask_at: Instant,
    whole: bool,
    /// Whether the BMC has no picture.
    dark: bool,
    /// Whether we logged a picture we couldn't decode, and a message we don't know, which once a session is enough.
    corrupt: bool,
    unknown: bool,
}

/// Keys and the mouse: the usage each held key went down as, whether our presses turned Caps Lock on, the mouse as we
/// last sent it.
struct Keyboard {
    sender: Sender,
    held: HashMap<String, u32>,
    caps: bool,
    pointer: (u8, u16, u16),
}

const SECOND: Duration = Duration::from_secs(1);

impl Client {
    /// Opens the BMC's screen, in a web session of its own.
    pub async fn kvm(&mut self) -> Result<Kvm> {
        let Ikvm { web, receiver, sender, width, height } = Ikvm::open(&self.connection).await?;
        // A placeholder the first update corrects (the X10's is 640 × 480), which we don't take beyond what fits.
        let (width, height) = if fits(width, height) { (width, height) } else { (640, 480) };
        let screen = Screen {
            receiver,
            decoder: Decoder::new(width, height),
            width,
            height,
            ask_at: Instant::now(),
            whole: true,
            dark: false,
            corrupt: false,
            unknown: false,
        };
        let keyboard = Keyboard { sender, held: HashMap::new(), caps: false, pointer: (0, 0, 0) };
        Ok(Kvm { screen: Mutex::new(screen), keyboard: Mutex::new(keyboard), web })
    }
}

impl Kvm {
    /// What changed on screen within a second, as `amt::Kvm::frame` says it. The BMC answers every request, mostly
    /// that nothing changed, about 25 times a second.
    pub async fn frame(&self) -> Result<Vec<u8>> {
        let mut screen = self.screen.lock().await;
        let deadline = Instant::now() + SECOND;
        loop {
            // Reading a message's first byte gives up nothing when it times out, and the rest follows at once.
            let Ok(kind) = tokio::time::timeout_at(deadline.min(screen.ask_at), screen.receiver.read(1)).await else {
                if Instant::now() >= deadline {
                    return Ok(screen.frame(&[]));
                }
                // Not before `ask_at`, so one request is on its way at a time, as the viewer keeps it.
                self.ask(&mut screen).await?;
                continue;
            };
            let receiver = &mut screen.receiver;
            match kind?[0] {
                0 => {
                    // FramebufferUpdate: one rectangle, its position ignored as the picture addresses the whole screen.
                    let header = receiver.read(23).await?;
                    let field = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
                    let [width, height] = [7, 9].map(|at| u16::from_be_bytes([header[at], header[at + 1]]));
                    let len = field(19) as usize;
                    if len > 8 << 20 {
                        return Err(Error::Protocol(format!("a {len}-byte screen update")));
                    }
                    let picture = receiver.read(len).await?;
                    screen.whole = false;
                    if picture.is_empty() {
                        // No signal: black once, as the viewer clears its screen, and a second before we ask again.
                        screen.ask_at = Instant::now() + SECOND;
                        if std::mem::replace(&mut screen.dark, true) {
                            continue;
                        }
                        screen.decoder = Decoder::new(screen.width, screen.height);
                        return Ok(screen.frame(&[[0, 0, screen.width, screen.height]]));
                    }
                    if field(11) != 87 {
                        self.ask(&mut screen).await?;
                        continue; // not AST2100, which is all the viewer decodes
                    }
                    let (width, height) =
                        if fits(width, height) { (width, height) } else { (screen.width, screen.height) };
                    if (width, height) != (screen.width, screen.height) || screen.dark {
                        (screen.decoder, screen.width, screen.height) = (Decoder::new(width, height), width, height);
                        (screen.whole, screen.dark) = (true, false);
                        self.keyboard.lock().await.sender.sync_mouse().await?;
                    }
                    let (painted, error) = screen.decoder.decode(&picture);
                    if let Some(error) = error
                        && !std::mem::replace(&mut screen.corrupt, true)
                    {
                        log::warn!("{error}, the rest of that update stays as it was");
                    }
                    // The next one's on its way while this one is painted.
                    self.ask(&mut screen).await?;
                    if !painted.is_empty() {
                        return Ok(screen.frame(&painted));
                    }
                }
                2 => {} // the Bell
                4 => {
                    // The cursor, which the picture shows anyway.
                    let header = receiver.read(20).await?;
                    let field = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
                    if field(16) == 1 {
                        let pixels = u64::from(field(8)) * u64::from(field(12));
                        if pixels > 128 * 128 {
                            return Err(Error::Protocol(format!("a cursor of {pixels} pixels")));
                        }
                        receiver.read(4 + 2 * pixels as usize).await?;
                    }
                }
                0x16 => _ = receiver.read(1).await?, // a keep-alive
                0x37 => _ = receiver.read(3).await?, // the mouse mode
                0x39 => {
                    // Someone joined or left, or the BMC ends our session, by a code in decimal digits.
                    let message = receiver.read(264).await?;
                    let why = match message[4..8].iter().fold(0, |code, &digit| code * 10 + u32::from(digit)) {
                        3 => "its web session was logged out",
                        4 => "too many KVM sessions",
                        8 => "a BIOS update",
                        9 => "a firmware update",
                        _ => continue,
                    };
                    return Err(Error::Ended(why));
                }
                kind => {
                    // As the viewer does, we drop what came with it and carry on.
                    let dropped = receiver.skip();
                    if !std::mem::replace(&mut screen.unknown, true) {
                        let dropped = &dropped[..dropped.len().min(32)];
                        log::warn!("dropped an iKVM message we don't know: {kind:02x} {dropped:02x?}");
                    }
                }
            }
        }
    }

    /// Asks for the next update, and again a second later unless it came.
    async fn ask(&self, screen: &mut Screen) -> Result<()> {
        self.keyboard.lock().await.sender.update(screen.whole, screen.width, screen.height).await?;
        screen.ask_at = Instant::now() + SECOND;
        Ok(())
    }

    pub async fn input(&self, inputs: Vec<Input>) -> Result<()> {
        for input in inputs {
            let Input::Text { text } = input else {
                self.keyboard.lock().await.input(input).await?;
                continue;
            };
            // At a person's pace so the host keeps up, the keyboard let go between characters: closing doesn't wait
            // for the rest, which then goes nowhere.
            for char in text.chars() {
                self.keyboard.lock().await.input(Input::Text { text: char.into() }).await?;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        Ok(())
    }

    /// Lets go of what's held and closes the WebSocket, then once a frame still on its way is in waits for the BMC to
    /// close it too, then always logs out of the web session, which the BMC would otherwise keep until it times out.
    pub async fn close(&self) {
        let goodbye = async {
            {
                let mut keyboard = self.keyboard.lock().await;
                let _ = keyboard.input(Input::Release).await;
                keyboard.sender.close().await;
            }
            self.screen.lock().await.receiver.closed().await;
        };
        let _ = tokio::time::timeout(GOODBYE, goodbye).await;
        self.web.logout().await;
    }
}

impl Screen {
    /// The screen's size, then `rects` with their pixels, as `amt::Kvm::frame` says it.
    fn frame(&self, rects: &[[u16; 4]]) -> Vec<u8> {
        let mut frame = [self.width.to_le_bytes(), self.height.to_le_bytes()].concat();
        for &[x, y, w, h] in rects {
            frame.extend([x, y, w, h].iter().flat_map(|value| value.to_le_bytes()));
            for row in y..y + h {
                let at = 4 * (usize::from(row) * usize::from(self.width) + usize::from(x));
                frame.extend_from_slice(&self.decoder.image()[at..at + 4 * usize::from(w)]);
            }
        }
        frame
    }
}

impl Keyboard {
    async fn input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Key { code, down: true } => {
                if let Some(usage) = usage(&code) {
                    // Caps Lock goes down as itself turning it on and as 0xFF39 turning it off, as the viewer sends it.
                    self.caps ^= usage == CAPS_LOCK;
                    let off = usage == CAPS_LOCK && !self.caps;
                    self.sender.key(if off { 0xFF00 | usage } else { usage }, true).await?;
                    self.held.insert(code, usage);
                }
            }
            Input::Key { code, down: false } => {
                if let Some(usage) = self.held.remove(&code) {
                    self.sender.key(usage, false).await?;
                }
            }
            Input::Pointer { x, y, buttons, scroll } => {
                // The DOM's buttons are left, right and middle, RFB's left, middle and right. A notch of the wheel is
                // a click of button 4 (up) or 5 (down).
                let buttons = buttons & 1 | (buttons & 2) << 1 | (buttons & 4) >> 1;
                if scroll != 0 {
                    self.sender.pointer(buttons | if scroll < 0 { 8 } else { 16 }, x, y).await?;
                }
                self.sender.pointer(buttons, x, y).await?;
                self.pointer = (buttons, x, y);
            }
            Input::Text { text } => {
                // As typed on a US keyboard, Shift held for the shifted ones.
                for at in text.chars().filter(|&char| char != '\0').filter_map(|char| US.find(char)) {
                    let usage = 4 + (at % 53) as u32;
                    let keys = [(SHIFT, true), (usage, true), (usage, false), (SHIFT, false)];
                    for &(usage, down) in if at < 53 { &keys[1..3] } else { &keys[..] } {
                        self.sender.key(usage, down).await?;
                    }
                }
            }
            Input::Release => {
                for usage in std::mem::take(&mut self.held).into_values() {
                    self.sender.key(usage, false).await?;
                }
                // The mouse where it was, its buttons up.
                let (buttons, x, y) = self.pointer;
                if buttons != 0 {
                    self.sender.pointer(0, x, y).await?;
                    self.pointer = (0, x, y);
                }
            }
        }
        Ok(())
    }
}

/// Whether the viewer takes a screen this size: it keeps the size it has for a larger one.
fn fits(width: u16, height: u16) -> bool {
    width <= 1920 && height <= 1280
}

const CAPS_LOCK: u32 = 0x39;
const SHIFT: u32 = 225;

/// DOM `code`s by their USB HID usage (the keyboard page) from 4, `-` for 0x32, the non-US # key, which has no code.
const KEYS: &str = "KeyA KeyB KeyC KeyD KeyE KeyF KeyG KeyH KeyI KeyJ KeyK KeyL KeyM KeyN KeyO KeyP KeyQ KeyR KeyS KeyT \
    KeyU KeyV KeyW KeyX KeyY KeyZ Digit1 Digit2 Digit3 Digit4 Digit5 Digit6 Digit7 Digit8 Digit9 Digit0 Enter Escape \
    Backspace Tab Space Minus Equal BracketLeft BracketRight Backslash - Semicolon Quote Backquote Comma Period Slash \
    CapsLock F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 PrintScreen ScrollLock Pause Insert Home PageUp Delete End \
    PageDown ArrowRight ArrowLeft ArrowDown ArrowUp NumLock NumpadDivide NumpadMultiply NumpadSubtract NumpadAdd \
    NumpadEnter Numpad1 Numpad2 Numpad3 Numpad4 Numpad5 Numpad6 Numpad7 Numpad8 Numpad9 Numpad0 NumpadDecimal \
    IntlBackslash ContextMenu";
/// From usage 224.
const MODIFIERS: &str = "ControlLeft ShiftLeft AltLeft MetaLeft ControlRight ShiftRight AltRight MetaRight";

/// What a US keyboard's keys from usage 4 to 56 type, unshifted then shifted, `\0` where they type nothing.
const US: &str = concat!(
    "abcdefghijklmnopqrstuvwxyz1234567890\n\0\0\t -=[]\\\0;'`,./",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\0\0\0\0\0_+{}|\0:\"~<>?",
);

fn usage(code: &str) -> Option<u32> {
    let find = |keys: &str, first| Some(first + keys.split_whitespace().position(|key| key == code)? as u32);
    find(KEYS, 4).or_else(|| find(MODIFIERS, 224))
}
