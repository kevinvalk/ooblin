# Mock devices

AMT lives in the management engine of a real vPro machine and nobody makes an emulator for it. So we record real devices once and replay them from then on.

```sh
cargo run -p mock -- record <fixture> <host>   # asks for the password, writes fixtures/<fixture>
cargo run -p mock -- run <fixture>             # serves it on 127.0.0.1 as admin / P@ssw0rd
```

- `record` reads everything the device offers without changing anything: every WS-Man class it knows, the read-only methods the app uses (clock, accounts, event and audit log) and a short serial console, KVM and IDER session.
- `run` fills in by the spec what a recording cannot hold: handshakes, authentication, method results, enumeration pages (AMT keeps one context for all of them), a KVM screen that waits for the viewer to ask for each update and a serial console that waits for you to type.
- The mock hangs up on a client that says anything else than was recorded (except the keys and pointer between the updates of a screen, and what is typed on the console). That way every transcript checks our bytes.
- `cargo test` replays every fixture, so whatever a device once told us keeps parsing. It also replays each fixture with a few files garbled, so whatever a device says we neither crash nor wait forever. A pull request runs 8 rounds of that, and CI 200 next to it (`GARBLED=200 cargo test --test garbled`).
- `run` also takes a path, for a fixture of your own. `record <fixture> <host> --sol` records only the serial console for as long as you like: make the device write to it (mount a disk image, say), then press Enter.

A BMC is recorded the same way: its Redfish (scrubbed, in the mockup layout of DMTF under `redfish/`), its IPMI (scrubbed, every answer our reads asked for in `ipmi.txt`) and, for a Supermicro, a short KVM session. `run x10` serves it on 127.0.0.1:16443 next to an AMT fixture: Redfish, the web login that the token of the KVM comes from, the WebSocket of the KVM and the CGI of its dashboard (`cgi/`). On UDP 623 it answers IPMI as the recording does, and as a BMC does for what a recording cannot hold: logins, the chassis commands, and a serial console that greets with a login prompt and echoes what is typed. A fixture without an `ipmi.txt` is a BMC with IPMI off.

IMPORTANT: Fixtures are committed, so only record a device that has nothing to hide! Serials, addresses and names are scrubbed, but the audit log keeps its user names and client addresses, and the KVM session keeps a screenshot of whatever is on screen.

The fixtures that are not our own devices:

- `fixtures/go-wsman-messages` is an untouched copy of the sample responses of Intel from [go-wsman-messages](https://github.com/device-management-toolkit/go-wsman-messages) (`pkg/wsman/wsmantesting/responses` at `00402b9`, Apache-2.0), which the mock serves next to a fixture.
- `fixtures/ipmi-sim` is what the simulator of OpenIPMI, another IPMI 2.0 implementation, told ipmitool. `run ipmi-sim` serves a BMC that we know over IPMI alone.
- `fixtures/openbmc` is OpenBMC: the `obmc-phosphor-image-romulus.static.mtd` of its CI, booted in QEMU with the command below. We recorded its IPMI with `record openbmc 127.0.0.1 --user root` (0penBmc) and its Redfish at `127.0.0.1:2443`, where IPMI does not answer. So it is one fixture that a login finds over IPMI and that the Kind Redfish speaks to as well.
- `fixtures/dmtf-rackmount1` is an untouched copy of the sample rack server of DMTF from their [Redfish-Mockup-Server](https://github.com/DMTF/Redfish-Mockup-Server) (`public-rackmount1` at `23d21f9`, BSD-3-Clause, its license next to it): the own example of the standard, with IPMI off.

```sh
qemu-system-arm -M romulus-bmc -nographic \
  -drive file=obmc-phosphor-image-romulus.static.mtd,format=raw,if=mtd \
  -net nic -net user,hostfwd=udp::623-:623,hostfwd=tcp::2443-:443
```
