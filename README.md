# AMT Console

A desktop console for Intel AMT: power control, hardware inventory, the device's screen, keyboard and mouse (KVM) and
booting it from a disk image on your machine (IDER). A serial console is next. No server, no cloud: the app talks to
your devices directly, over TLS only, with pinned certificates and passwords kept in your OS keychain.

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

| Feature                                                      |        | Notes                                                                                                                                                                       |
| ------------------------------------------------------------ | ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Discovery, general info                                      | Partly | Version, control mode, host name, certificate. Not: provisioning record, ME capabilities, privacy level.                                                                    |
| Hardware asset                                               | Yes    | System, BIOS, processors, memory, disks, batteries. Not: FRU and media device tables.                                                                                       |
| Remote control (power, boot)                                 | Partly | Every power action, booting once to BIOS, PXE or a disk image. Not: HTTPS boot, One-Click Recovery, local boot sources, `UseSOL`, BIOS pause, input locks, OS power saving. |
| KVM                                                          | Yes    | ZRLE and Raw at 16 bpp, keyboard, mouse, typing text, full screen. Not: 8 bpp (AMT 18+ beyond 4K), display choice, KVM settings.                                            |
| SOL                                                          | Yes    | xterm.js at 80 by 25.                                                                                                                                                       |
| Storage redirection (IDER)                                   | Yes    | A CD-ROM from an ISO, the floppy left empty.                                                                                                                                |
| User consent                                                 | No     | We say when it's needed, we can't ask for it.                                                                                                                               |
| Redirection and KVM switches                                 | Partly | Turned on when needed, not by hand.                                                                                                                                         |
| Event manager (event log)                                    | Partly | Reading. Not: export, clear, freeze, filters, alerts, subscriptions.                                                                                                        |
| Access monitor (audit log)                                   | Partly | Reading. Not: export, clear, lock, auditing on or off, policy.                                                                                                              |
| Alarm clock                                                  | No     |                                                                                                                                                                             |
| Time synchronization                                         | Partly | Reading the clock. Not: setting it.                                                                                                                                         |
| Network administration                                       | Partly | Reading wired IPv4 and IPv6. Not: changing them, host name, link preference and protection.                                                                                 |
| Wi-Fi port configuration                                     | No     |                                                                                                                                                                             |
| 802.1X profiles                                              | No     |                                                                                                                                                                             |
| Access control list (users)                                  | Partly | Reading users. Not: adding, changing, removing, the admin password.                                                                                                         |
| Certificate management                                       | No     |                                                                                                                                                                             |
| Transport layer security                                     | Partly | TLS only, pinned. Not: reading or changing AMT's TLS settings.                                                                                                              |
| Remote platform erase                                        | No     |                                                                                                                                                                             |
| Environment detection                                        | No     |                                                                                                                                                                             |
| Kerberos                                                     | No     | Not planned.                                                                                                                                                                |
| Remote access (CIRA), Fast Call for Help                     | No     | Not planned: needs a server.                                                                                                                                                |
| WS-Eventing, alerts                                          | No     | Not planned: needs a listener.                                                                                                                                              |
| System defense, agent presence                               | No     | Not planned.                                                                                                                                                                |
| Third-party storage, web application hosting, power packages | No     | Not planned: deprecated.                                                                                                                                                    |

## TODO

- [ ] Show which parts of a disk image the device reads: a strip of cells in the footer, one per stretch of the image, lit as it's read and fading after. Maybe also show a way on max speed IDE-R supports which in theory according intel caps out to 2 MB/s
- [ ] Have an optional built in PXE boot system where our tool hosts the PXE server directly.
- [ ] A settings UI that changes things, by card: view, then Edit with Save and Cancel, asking first where a change can cut
      the connection or lose data.
- [ ] Then the features above, starting with: redirection switches and the consent policy, user consent, setting the AMT
      clock, `UseSOL` and the other boot options, the alarm clock, exporting and clearing the logs, users and the admin
      password, certificates and TLS (reading first). Network and Wi-Fi last, as a wrong setting cuts us off.
- [ ] KVM at 8 bpp, for screens beyond 4K on AMT 18 and later.
- [ ] IDER: READ (12) and REQUEST SENSE, and checking that a plain reset after booting the image boots from disk again
      (clearing `UseIDER` if not).
- [ ] Check on the MS-02 that SOL survives 10 minutes idle.
- [ ] Write down the KVM latency (3 to 4 ms for a 1024 by 768 frame in a release build) and measure a 1080p screen with a
      window being dragged.
- [ ] Say in the README that ⌘Tab and other system shortcuts can't reach the device.
- [ ] `withGlobalTauri` off in release builds, the verify loop then needs another way in.
- [ ] CI: tests, clippy, the UI build, and releases with tauri-action, SHA-256 sums and a provenance attestation.
- [ ] Build from a fresh clone on macOS, Linux and Windows.
