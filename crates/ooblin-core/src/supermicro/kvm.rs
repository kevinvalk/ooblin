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
    decoder: Decoder,

    /// When we ask again for an update that has not come: a second after we asked, or a second after the BMC said it
    /// has no picture (the host's screen is off, say), as the viewer does.
    ask_at: Instant,
    is_whole: bool,
    is_dark: bool, // The BMC has no picture

    /// We log a picture we could not decode and a message we do not know only once a session, which is enough.
    has_logged_corrupt: bool,
    has_logged_unknown: bool,
}

struct Keyboard {
    sender: Sender,
    held: HashMap<String, u32>, // The usage each held key went down as
    is_caps_lock_on: bool,      // As far as our own presses turned it on
    pointer: (u8, u16, u16),    // The mouse as we last sent it
}

const SECOND: Duration = Duration::from_secs(1);

impl Client {
    /// Opens the BMC's screen, in a web session of its own.
    pub async fn kvm(&mut self) -> Result<Kvm> {
        let Ikvm { web, receiver, sender, width, height } = Ikvm::open(&self.redfish.connection).await?;

        // A placeholder the first update corrects (the X10's is 640 × 480), and we do not take one beyond what fits.
        let (width, height) = if fits(width, height) { (width, height) } else { (640, 480) };

        let screen = Screen {
            receiver,
            decoder: Decoder::new(width, height),
            ask_at: Instant::now(),
            is_whole: true,
            is_dark: false,
            has_logged_corrupt: false,
            has_logged_unknown: false,
        };
        let keyboard = Keyboard { sender, held: HashMap::new(), is_caps_lock_on: false, pointer: (0, 0, 0) };

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

                // We do not ask before `ask_at`, so one request is on its way at a time, as the viewer keeps it.
                self.ask(&mut screen).await?;
                continue;
            };

            // NOTE: ATEN's iKVM is RFB with messages of its own and the screen in AST2100 video. We handle each message
            // as the BMC's own HTML5 viewer does.
            let receiver = &mut screen.receiver;
            match kind?[0] {
                0 => {
                    // FramebufferUpdate: one rectangle, its position ignored as the picture addresses the whole screen.
                    let header = receiver.read(23).await?;
                    let field = |offset: usize| u32::from_be_bytes(header[offset..offset + 4].try_into().unwrap());
                    let [width, height] = [7, 9].map(|offset| u16::from_be_bytes([header[offset], header[offset + 1]]));
                    let length = field(19) as usize;
                    if length > 8 << 20 {
                        return Err(Error::Protocol(format!("a {length}-byte screen update")));
                    }

                    let picture = receiver.read(length).await?;
                    screen.is_whole = false;

                    if picture.is_empty() {
                        // No signal: we paint black once, as the viewer clears its screen, and ask again in a second.
                        screen.ask_at = Instant::now() + SECOND;
                        if std::mem::replace(&mut screen.is_dark, true) {
                            continue;
                        }

                        let Decoder { width, height, .. } = screen.decoder;
                        screen.decoder = Decoder::new(width, height);
                        return Ok(screen.frame(&[[0, 0, width, height]]));
                    }

                    // The viewer decodes AST2100 (encoding 87) only, and so do we.
                    if field(11) != 87 {
                        self.ask(&mut screen).await?;
                        continue;
                    }

                    let size = (screen.decoder.width, screen.decoder.height);
                    let (width, height) = if fits(width, height) { (width, height) } else { size };
                    if (width, height) != size || screen.is_dark {
                        screen.decoder = Decoder::new(width, height);
                        (screen.is_whole, screen.is_dark) = (true, false);
                        self.keyboard.lock().await.sender.sync_mouse().await?;
                    }

                    let (painted, error) = screen.decoder.decode(&picture);
                    if let Some(error) = error
                        && !std::mem::replace(&mut screen.has_logged_corrupt, true)
                    {
                        log::warn!("{error}, the rest of that update stays as it was");
                    }

                    // We ask now, so the next update is on its way while this one is painted.
                    self.ask(&mut screen).await?;
                    if !painted.is_empty() {
                        return Ok(screen.frame(&painted));
                    }
                }
                2 => {} // The bell
                4 => {
                    // The cursor, which we skip because the picture shows it anyway.
                    let header = receiver.read(20).await?;
                    let field = |offset: usize| u32::from_be_bytes(header[offset..offset + 4].try_into().unwrap());
                    if field(16) == 1 {
                        let pixels = u64::from(field(8)) * u64::from(field(12));
                        if pixels > 128 * 128 {
                            return Err(Error::Protocol(format!("a cursor of {pixels} pixels")));
                        }

                        receiver.read(4 + 2 * pixels as usize).await?;
                    }
                }
                0x16 => _ = receiver.read(1).await?, // A keep-alive
                0x37 => _ = receiver.read(3).await?, // The mouse mode
                0x39 => {
                    // Someone joined or left, or the BMC ends our session, by a code in four bytes that each hold one
                    // decimal digit as its value.
                    // TODO: We never saw a real X10 end a session, so check that these are not ASCII digits?!
                    let message = receiver.read(264).await?;
                    let reason = match message[4..8].iter().fold(0, |code, &digit| code * 10 + u32::from(digit)) {
                        3 => "its web session was logged out",
                        4 => "too many KVM sessions",
                        8 => "a BIOS update",
                        9 => "a firmware update",
                        _ => continue,
                    };

                    return Err(Error::Ended(reason));
                }
                kind => {
                    // As the viewer does, we drop what came with it and carry on.
                    let dropped = receiver.skip();
                    if !std::mem::replace(&mut screen.has_logged_unknown, true) {
                        let dropped = &dropped[..dropped.len().min(32)];
                        log::warn!("dropped an iKVM message we don't know: {kind:02x} {dropped:02x?}");
                    }
                }
            }
        }
    }

    /// Asks for the next update, and again a second later unless it came.
    async fn ask(&self, screen: &mut Screen) -> Result<()> {
        self.keyboard.lock().await.sender.update(screen.is_whole, screen.decoder.width, screen.decoder.height).await?;
        screen.ask_at = Instant::now() + SECOND;

        Ok(())
    }

    pub async fn input(&self, inputs: Vec<Input>) -> Result<()> {
        for input in inputs {
            let Input::Text { text } = input else {
                self.keyboard.lock().await.input(input).await?;
                continue;
            };

            // We type at a person's pace so the host keeps up, and let go of the keyboard between characters: that way
            // closing does not wait for the rest, which then goes nowhere.
            for character in text.chars() {
                self.keyboard.lock().await.input(Input::Text { text: character.into() }).await?;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        Ok(())
    }

    /// We let go of what is held and close the WebSocket, then wait for the BMC to close it too once a frame still on
    /// its way is in. We ALWAYS log out of the web session, as the BMC would otherwise keep it until it times out.
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
    /// The screen's size, then `rectangles` with their pixels, as `amt::Kvm::frame` says it.
    fn frame(&self, rectangles: &[[u16; 4]]) -> Vec<u8> {
        let screen_width = self.decoder.width;
        let mut frame = [screen_width.to_le_bytes(), self.decoder.height.to_le_bytes()].concat();

        for &[x, y, width, height] in rectangles {
            frame.extend([x, y, width, height].iter().flat_map(|value| value.to_le_bytes()));
            for row in y..y + height {
                let offset = 4 * (usize::from(row) * usize::from(screen_width) + usize::from(x));
                frame.extend_from_slice(&self.decoder.image()[offset..offset + 4 * usize::from(width)]);
            }
        }

        frame
    }
}

impl Keyboard {
    async fn input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Key { code, is_down: true } => {
                if let Some(usage) = usage(&code) {
                    // Caps Lock goes down as itself turning it on and as 0xFF39 turning it off, as the viewer sends it.
                    self.is_caps_lock_on ^= usage == CAPS_LOCK;
                    let is_turning_off = usage == CAPS_LOCK && !self.is_caps_lock_on;
                    self.sender.key(if is_turning_off { 0xFF00 | usage } else { usage }, true).await?;
                    self.held.insert(code, usage);
                }
            }
            Input::Key { code, is_down: false } => {
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
                // We type as on a US keyboard, with Shift held for the shifted characters.
                let indexes =
                    text.chars().filter(|&character| character != '\0').filter_map(|character| US.find(character));
                for index in indexes {
                    let usage = 4 + (index % 53) as u32;
                    let keys = [(SHIFT, true), (usage, true), (usage, false), (SHIFT, false)];
                    for &(usage, is_down) in if index < 53 { &keys[1..3] } else { &keys[..] } {
                        self.sender.key(usage, is_down).await?;
                    }
                }
            }
            Input::Release => {
                for usage in std::mem::take(&mut self.held).into_values() {
                    self.sender.key(usage, false).await?;
                }

                // The mouse stays where it was, with its buttons up.
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
