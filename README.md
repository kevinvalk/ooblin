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

AMT lives in the management engine of a real vPro machine, and nobody makes an emulator for it. So we record real
devices once and replay them from then on: `mock-amt record <fixture> <host>` asks for the password and reads everything
the device offers, without changing anything: every WS-Man class it knows, the read-only methods the app uses (clock,
accounts, event and audit log) and a short serial console, KVM and IDER session, all into `fixtures/<fixture>`.
`mock-amt run <fixture>` then serves it on 127.0.0.1 as `admin` / `P@ssw0rd`, filling in by the spec what a recording
can't hold: handshakes, authentication, method results. `cargo test` replays every fixture, so whatever a device once
told us keeps parsing.

Fixtures are committed, so record a device that has nothing to hide. Serials, addresses and names are scrubbed, but the
audit log keeps its user names and client addresses, and the KVM session keeps a screenshot of whatever is on screen.
`fixtures/go-wsman-messages` is an untouched copy of Intel's sample responses from
[go-wsman-messages](https://github.com/device-management-toolkit/go-wsman-messages) (`pkg/wsman/wsmantesting/responses`
at `00402b9`, Apache-2.0), which the mock serves alongside.
