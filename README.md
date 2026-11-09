<img src="assets/ooblin.svg" alt="" width="96" align="right">

# Ooblin

Ooblin, the out-of-band (OOB) goblin, is a desktop console for Intel AMT: power control, hardware inventory, the
device's screen, keyboard and mouse (KVM), its serial console and booting it from a disk image on your machine (IDER).
No server, no cloud: the app talks to your devices directly, over TLS only, with pinned certificates and passwords kept
in your OS keychain.

Built with [Tauri](https://tauri.app) for Linux, macOS and Windows.

## Run

`cargo release` bundles the app (`cargo tauri build`, set up for shipping). Start it with a host to connect straight
away, using its saved credentials or the ones you pass, in the Ooblin that's already running if there is one:

```sh
ooblin [host] [--user admin] [--password <password>]
```

Ooblin logs what it does and what goes wrong, a megabyte and the one before, in your OS's log folder. The button at the
right of the status bar saves it for a bug report: it has your devices' addresses and names, never a password.

## Develop

With the Rust of `rust-toolchain.toml`, the Node of `.node-version` and the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) in place (CI checks every push on Linux, macOS and
Windows the same way):

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
can't hold: handshakes, authentication, method results, enumeration pages (AMT keeps one context for all of them), a
KVM screen that waits for the viewer to ask for each update and a serial console that waits for you to type. It hangs
up on a client that says anything else than was recorded, so every transcript checks our bytes. `cargo test` replays
every fixture, so whatever a device once told us keeps parsing. `mock-amt run` also takes a path, for a fixture of
your own, and `mock-amt record <fixture> <host> --sol` records only the serial console, for as long as you like: make
the device write to it (mount a disk image, say), then press Enter.

Fixtures are committed, so record a device that has nothing to hide. Serials, addresses and names are scrubbed, but the
audit log keeps its user names and client addresses, and the KVM session keeps a screenshot of whatever is on screen.
`fixtures/go-wsman-messages` is an untouched copy of Intel's sample responses from
[go-wsman-messages](https://github.com/device-management-toolkit/go-wsman-messages) (`pkg/wsman/wsmantesting/responses`
at `00402b9`, Apache-2.0), which the mock serves alongside.

## What it supports

By the features in Intel's [AMT Implementation and Reference
Guide](https://software.intel.com/sites/manageability/AMT_Implementation_and_Reference_Guide/default.htm), as checked
against the MS-02 (AMT 19). Partly: some of it, see the notes.

| Feature                                      |        | Notes                                                                                                                                                              |
| -------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Discovery, general info                      | Partly | Finding devices by RMCP ping, version, control mode, host name, certificate. Not: provisioning record, ME capabilities, privacy level.                             |
| Hardware asset                               | Yes    | System, BIOS, processors, memory, disks, batteries. Not: FRU and media device tables.                                                                              |
| Remote control (power, boot)                 | Partly | Every power action, booting once to BIOS, PXE, a disk image or with the BIOS on SOL. Not: One-Click Recovery's boots, local boot sources, BIOS pause, input locks. |
| KVM                                          | Yes    | ZRLE and Raw, 16-bit and 8-bit beyond 4K as AMT wants, keyboard, mouse, typing text, full screen, picking a screen, frames a second. Not: KVM settings.            |
| SOL                                          | Yes    | xterm.js at 80 by 25, Backspace sent as ^H like MeshCommander.                                                                                                     |
| Storage redirection (IDER)                   | Yes    | A CD-ROM from an ISO, the floppy left empty.                                                                                                                       |
| User consent                                 | Yes    | Choosing when it's needed, and asking for it: the device shows a code someone there reads out.                                                                     |
| Redirection and KVM switches                 | Yes    | By hand in the settings, sessions open only what's on.                                                                                                             |
| Event manager (event log)                    | Partly | Reading. Not: export, clear, freeze, filters, alerts, subscriptions.                                                                                               |
| Access monitor (audit log)                   | Partly | Reading. Not: export, clear, lock, auditing on or off, policy.                                                                                                     |
| Alarm clock                                  | Yes    | Scheduling wake-ups (once, hourly, daily, weekly) and canceling them.                                                                                              |
| Time synchronization                         | Yes    | Reading the clock, setting it to this computer's.                                                                                                                  |
| Network administration                       | Partly | Wired IPv4 and IPv6, when it's reachable, the name, DNS registration, ping. Not: wireless.                                                                         |
| Wi-Fi port configuration                     | No     |                                                                                                                                                                    |
| 802.1X profiles                              | No     |                                                                                                                                                                    |
| Access control list (users)                  | No     | Left to AMT's own web pages, which we link to.                                                                                                                     |
| Certificate management                       | Partly | Listing, adding certificates and trusted roots, saving, deleting. Not: keys, signing requests.                                                                     |
| Transport layer security                     | Partly | TLS only, pinned, its settings shown. Not: changing them or its certificate.                                                                                       |
| Remote platform erase                        | Partly | Turning it and One-Click Recovery on and off where the BIOS allows. Not: erasing (neither on the MS-02).                                                           |
| Environment detection                        | No     |                                                                                                                                                                    |
| Kerberos                                     | No     | Not planned.                                                                                                                                                       |
| Remote access (CIRA), Fast Call for Help     | No     | Not planned: needs a server.                                                                                                                                       |
| WS-Eventing, alerts                          | No     | Not planned: needs a listener.                                                                                                                                     |
| System defense, agent presence               | No     | Not planned.                                                                                                                                                       |
| Power packages (power policy)                | Yes    | Choosing which of the maker's the device uses, and how long AMT stays awake after.                                                                                 |
| Third-party storage, web application hosting | No     | Not planned: deprecated.                                                                                                                                           |

## TODO

- [ ] Have an optional built in PXE boot system where our tool hosts the PXE server directly.
- [ ] The features above, starting with: the other boot options (One-Click Recovery's boots among them), exporting and clearing the logs, TLS's certificate, platform erase itself. Wi-Fi last.
- [ ] CI: tests, clippy, the UI build, and releases with tauri-action (as `cargo release` builds them), SHA-256 sums and a provenance attestation.
- [ ] Build from a fresh clone on macOS, Linux and Windows.
