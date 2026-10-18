# AMT Console

A desktop console for Intel AMT. Power control and hardware inventory today, remote desktop, serial console and ISO
boot next. No server, no cloud: the app talks to your devices directly, over TLS only, with pinned certificates and
passwords kept in your OS keychain.

Built with [Tauri](https://tauri.app) for Linux, macOS and Windows.

## Run

`cargo tauri build` bundles the app. Start it with a host to connect straight away, using its saved credentials or the
ones you pass:

```sh
amt-console [host] [--user admin] [--password <password>]
```

## Develop

With Rust, Node and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) in place:

```sh
cargo install tauri-cli --locked
npm install --prefix ui
cargo tauri dev            # or cargo tauri dev -- -- <host>
cargo test
```

### Mock AMT

AMT lives in the management engine of a real vPro machine, and nobody makes an emulator for it. So we record a real
device once and replay it from then on. `mock-amt record <fixture> <host>` asks for the password and reads everything
the device offers, without changing anything: every WS-Man class it knows (serials, addresses and names scrubbed) and a
short serial console, KVM and IDER session, all into `fixtures/<fixture>`. `mock-amt run <fixture>` then serves it on
127.0.0.1 as `admin` / `P@ssw0rd`, filling in by the spec what a recording can't hold: handshakes, authentication,
method results. `cargo test` replays every fixture, so whatever a device once told us keeps parsing.

`fixtures/go-wsman-messages` holds Intel's own sample responses, fixtures of real devices stay out of git. Run it with
`cargo run -p mock-amt -- run go-wsman-messages`.
