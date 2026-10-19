# AMT Console PoC Handover

Oct 18, 2026

## Goal

Build a proof of concept of a desktop Intel AMT console as a Tauri v2 app: Rust owns every socket, the webview reuses existing open-source AMT UI code, nothing runs as a server and no port is opened. Target: manage a Minisforum MS-02 Ultra (AMT 19) from macOS with hardware info, power control, SOL, KVM and IDER.

Constraints:

- Rust backend, Tauri v2, macOS on Apple Silicon first. Linux and Windows builds are welcome but not gating.
- All network I/O lives in one module tree (`amt-core::net`) so the egress surface is reviewable in one place.
- No server component, no database, no telemetry, no updater plugin, no cloud. Hosts and TLS pins in a local JSON file, passwords in the macOS keychain.
- Reuse existing protocol code where it exists (section 5) instead of re-deriving it from the Intel SDK. Everything reused is Apache-2.0; keep attribution.
- Prove every protocol step in a CLI before wiring it into the UI.

Non-goals for the PoC: CIRA/MPS, AMT provisioning (rpc-go stays the tool for that), Kerberos, multi-user, code signing and notarization, Windows IPC tuning, the legacy VNC port 5900 (removed from current firmware).

## Target environment

The test target is one real box; there is no AMT emulator, so every milestone is verified against it.

- Device: Minisforum MS-02 Ultra, Core Ultra 9 285HX, Intel AMT 19 (CSME 19.0.5 after BIOS 1.04), wired LAN only.
- AMT 19 is TLS-only. WS-Man is `https://<host>:16993/wsman`; KVM, SOL and IDER are TCP 16995 over TLS. Ports 16992 and 16994 are closed on this firmware. Do not write plaintext code paths.
- The AMT certificate is self-signed. Pin it (trust on first use, SHA-256 of the DER leaf). Never ship `accept_invalid_certs`.
- Provisioning: admin control mode via MEBx, user consent set to None, KVM enabled. No opt-in code is needed. Implement the `IPS_OptInService` flow only if time permits.
- Authentication: HTTP Digest, user `admin`, realm of the form `Digest:<hex>`, qop=auth. The same credentials authenticate the redirection handshake on 16995.
- Known-good reference: Intel's Device Management Toolkit Console v1.46 does KVM, SOL and IDER against this exact box today, so a failure is in our code, not the firmware.
- Test credentials come from a gitignored `.env` in the repo root: `AMT_HOST`, `AMT_USER`, `AMT_PASS`. Never log the password or the Digest HA1.

## Architecture

Three crates in one Cargo workspace. The Tauri app is a thin shell over a library that is fully exercisable from a CLI without a webview.

&#91;embedded content: component map · 3 crates, 2 AMT ports\]

The webview never holds a socket: it calls Rust commands and receives post-handshake bytes; `amtctl` drives the same library for testing without a UI.

- `amt-core` (lib, tokio): TLS connector with pin store, Digest, WS-Man client (`post(body) -> xml`), redirection session handshake, KVM and SOL byte pumps, IDER device emulation. No Tauri dependency.
- `amtctl` (bin): CLI over `amt-core` for fast iteration: `amtctl info`, `amtctl power reset`, `amtctl sol`, `amtctl ider --iso file.iso --boot`, `amtctl kvm-snapshot out.png`. Every protocol milestone is proven here first.
- `amt-console` (Tauri v2 app): Rust commands wrap `amt-core`; the webview holds the host list, the RFB viewer and the SOL terminal.

Frontend: TypeScript with Vite, no framework for the PoC. Protocol code is ported from the donors in section 5; the agent writes glue, not protocols.

Transport boundary: Rust completes the AMT redirection handshake (section 4) and hands the webview a post-handshake byte stream. For KVM that stream starts with the RFB version string; for SOL it is the SOL payload channel. IDER never crosses into the webview.

## Protocol facts to get right

When a detail below disagrees with the reference code in section 5, the reference code wins; when both disagree with the MS-02, the firmware wins.

**WS-Man**

- POST `https://<host>:16993/wsman`, `Content-Type: application/soap+xml;charset=UTF-8`, explicit `Content-Length` (AMT rejects chunked transfer encoding). Reuse one connection; AMT allows only a few concurrent ones.
- Digest: expect 401 on the first request, answer with MD5, qop=auth, retry. Handle `stale=true` by re-challenging silently. `diqwest` on top of `reqwest` (rustls backend) does this; hand-rolling is about 40 lines if the crate fights the custom verifier.
- Envelopes: WS-Transfer Get/Put, WS-Enumeration Enumerate+Pull, and method invokes against `http://intel.com/wbem/wscim/1/amt-schema/1/<AMT_Class>`, `.../ips-schema/1/<IPS_Class>` and `http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/<CIM_Class>`. Take the exact bodies from `go-wsman-messages`; do not compose XML by intuition.
- Calls the PoC needs: `AMT_GeneralSettings` Get; `CIM_AssociatedPowerManagementService` Get (power state); `CIM_PowerManagementService.RequestPowerStateChange` (2 on, 8 off soft, 5 power cycle, 10 reset); `AMT_RedirectionService` Get, Put `ListenerEnabled=true`, `RequestStateChange(32771)` for IDER+SOL; `CIM_KVMRedirectionSAP.RequestStateChange(2)`; `IPS_KVMRedirectionSettingData` Get; `IPS_OptInService` Get (`OptInRequired`); `AMT_BootSettingData` Put (`UseIDER`, `IDERBootDevice`, `UseSOL`); `CIM_BootConfigSetting.ChangeBootOrder` with `Intel(r) AMT: Force CD/DVD Boot`; `CIM_BootService.SetBootConfigRole(1)`; inventory via `CIM_ComputerSystemPackage`, `CIM_Chassis`, `CIM_BIOSElement`, `CIM_Processor`, `CIM_PhysicalMemory`; `AMT_AuditLog.ReadRecords` optional.

**TLS**

- `tokio-rustls` with a custom `ServerCertVerifier`: compare SHA-256 of the leaf DER to the stored pin; first contact with no pin stores it and shows the fingerprint. Delegate signature checks to rustls' default verifier.
- Go's `crypto/tls` (ECDHE-only defaults since 1.22) connects to this firmware, so rustls should too. If the handshake fails with no shared cipher suite, record the ClientHello/ServerHello and try `native-tls` before changing anything else.

**Redirection port 16995**

- One TCP+TLS connection per session; KVM, SOL and IDER each open their own.
- Handshake: START\_REDIRECTION\_SESSION with the 4-byte type (`KVMR`, ` SOL  `, `IDER`), read the reply, then AUTHENTICATE\_SESSION: query supported auth types, request a Digest challenge, answer it (digest URI `/RedirectionService`), read success. Port the byte layouts from `amt-redir-ws-0.1.0.js`, a working implementation, and cross-check with the Intel SDK Redirection Library text. Never guess opcodes.
- After auth: KVM is plain RFB on the same socket (AMT uses Intel's `RFB 004.000` version string, not `003.008`; the security and init steps are in `amt-desktop-0.0.2.js`). SOL is START\_SOL followed by framed data messages. IDER is START\_IDER, after which AMT drives the session by sending ATAPI command packets that we answer as a CD-ROM.

**KVM / RFB**

- Encodings AMT accepts: Raw (0) and ZRLE (16) plus the DesktopSize pseudo-encoding; copy the exact SetEncodings list from `amt-desktop`. Negotiate 32 bpp first, 16 bpp as a bandwidth toggle.
- ZRLE uses one zlib stream for the whole session (`flate2`); resetting it per rectangle corrupts frames.
- Input: RFB KeyEvent takes X11 keysyms (MeshCommander's table covers macOS browsers); PointerEvent buttons are a bitmask, scroll is buttons 4 and 5.
- Backpressure: send the next incremental FramebufferUpdateRequest only after the previous update is painted.

**SOL**

- A VT100 byte stream; `xterm.js` in the webview, Rust only strips the SOL framing.
- BIOS setup over SOL needs `UseSOL` in `AMT_BootSettingData` plus a reset; otherwise nothing appears until the OS redirects its console.

**IDER**

- The console side is unchanged since AMT 11 even though the host now sees a USB device (USB-R). Emulate an ATAPI CD-ROM: INQUIRY, TEST UNIT READY, READ CAPACITY, READ(10)/READ(12), READ TOC, MODE SENSE(10), REQUEST SENSE, GET CONFIGURATION, START STOP UNIT, PREVENT ALLOW MEDIUM REMOVAL. The floppy slot reports no media.
- Sector size 2048. Transfer chunk sizes come from the START\_IDER reply; per-command round trips dominate, not bandwidth.
- Boot flow, in this order: enable IDER on `AMT_RedirectionService`, open the IDER session, Put `AMT_BootSettingData` (`UseIDER=true`, `IDERBootDevice=1` for CD), `ChangeBootOrder` to Force CD/DVD, `SetBootConfigRole(1)`, then `RequestPowerStateChange(10)` or `(2)`. The session must be up before the reset.
- Port `amt-ider.js` whole; it handles the AMT-specific command framing. Memory-map the ISO (`memmap2`) so a READ is a memcpy.

## Reference code

All protocol logic already exists in the open; the job is porting, not inventing. Order of trust when sources disagree: observed MS-02 behavior, then DMT (maintained, tested on AMT 19), then MeshCommander (last touched 2023), then the SDK text.

| What | Where | Use it for | License |
| --- | --- | --- | --- |
| Redirection handshake: START, AUTH, Digest over the socket | `amt-redir-ws-0.1.0.js` in [Ylianst/MeshCommander](https://github.com/Ylianst/MeshCommander) | Port to Rust 1:1 | Apache-2.0 |
| KVM: RFB state machine, ZRLE decode, keysym table | `amt-desktop-0.0.2.js` in MeshCommander | Port decode to Rust; reuse the input mapping in TS | Apache-2.0 |
| SOL framing and terminal | `amt-terminal-0.0.2.js` in MeshCommander | Port framing to Rust; replace the terminal with xterm.js | Apache-2.0 |
| IDER ATAPI emulation | `amt-ider.js` in MeshCommander | Port to Rust whole | Apache-2.0 |
| High-level AMT helpers (enable KVM, power, boot options) | `amt-0.2.0.js` in MeshCommander | Call order and parameter values | Apache-2.0 |
| WS-Man bodies for every AMT, IPS and CIM class | [go-wsman-messages](https://github.com/device-management-toolkit/go-wsman-messages) | Copy the XML templates for the calls in section 4 | Apache-2.0 |
| Current KVM, SOL, IDER components in TypeScript | [ui-toolkit](https://github.com/device-management-toolkit/ui-toolkit) | Second opinion when MeshCommander disagrees with the firmware | Apache-2.0 |
| A working app against AMT 19 | [console](https://github.com/device-management-toolkit/console) (Go) | Read the usecase layer for sequences; run it to compare behavior | Apache-2.0 |
| Official protocol text | [Intel AMT SDK: KVM Application Developers Guide](https://software.intel.com/sites/manageability/AMT_Implementation_and_Reference_Guide/WordDocuments/kvmapplicationdevelopersguide.htm) and the Redirection Library chapters beside it | Resolve ambiguity in the ports | Intel |

What not to use: MeshCommander's `webrelay.ashx` relay and `amt-wsman-comm-*.js` transport (replaced by Rust), its firmware edition (needs AMT web storage, gone since 16.1), and anything in the DMT that assumes MPS/RPS or CIRA.

## IPC and performance

Default path is Tauri v2 binary IPC: Rust to webview through a `Channel` carrying raw bytes, webview to Rust through `invoke` with a raw request body. A user benchmark quoted by the Tauri maintainers puts this at about 5 ms per 10 MB on macOS and about 200 ms on Windows, which is why Windows is out of scope. Verify early whether channel messages actually take the raw path or get JSON-encoded on the way; if the latter, go straight to lever 2.

Measure before optimizing, in M3: log socket-read-to-canvas-paint latency and CPU for a 1080p desktop with a window being dragged. Pick levers from the numbers:

1. Decode RFB in Rust, ship pixels. Rust keeps the framebuffer, decodes Raw and ZRLE, and sends only dirty rectangles as RGBA (a small x, y, w, h header plus bytes), coalesced per FramebufferUpdate. The webview does one `putImageData` per rect. This removes JS inflate and per-pixel loops and is the expected winner.
2. Pull instead of push. Register a custom URI scheme (`kvm://frame?seq=N`) with `register_uri_scheme_protocol` and let the webview `fetch` each frame as an ArrayBuffer. Fetch bodies are the cheapest path into WebKit.
3. Loopback WebSocket. `tokio-tungstenite` on 127.0.0.1, random port, per-launch token in the URL, binary frames. Both donor UIs already speak this contract. Costs one listening socket; keep it behind a feature flag and off by default.
4. Paint once per frame. Batch rect writes in `requestAnimationFrame`, move compositing to an `OffscreenCanvas` in a Worker if the main thread stalls, and request the next incremental update only after paint.
5. Negotiate less. 16 bpp halves bytes on the wire when the Mac is on Wi-Fi; expose it as a toggle.

IDER stays entirely in Rust. The protocol is request/response per command, so every IPC hop would add latency per sector. Memory-map the ISO and report progress to the UI as counters, never data.

SOL is tiny; send it over the Channel unchanged.

Tauri settings that matter: `app.security.csp` allows `connect-src ipc: http://ipc.localhost` (add `ws://127.0.0.1:*` only when lever 3 is on); the capabilities file lists `dialog:allow-open` for the ISO picker and the app's own commands; `withGlobalTauri` stays off.

## Milestones

Prove each step in `amtctl` before touching the UI. One commit per milestone, with a short note on what the firmware actually did when it differed from the references.

| M | Scope | Done when |
| --- | --- | --- |
| M0 | Workspace, `.env`, TLS connector with TOFU pin, Digest, `AMT_GeneralSettings` Get | `amtctl info` prints hostname and AMT version from the MS-02; a second run reuses the pin; a changed pin is refused with the fingerprint shown |
| M1 | Power state and control, hardware inventory, audit log | `amtctl power state` matches reality; `amtctl power reset` reboots the box; `amtctl inventory` lists CPU, DIMMs, BIOS version |
| M2 | Redirection handshake, SOL | `amtctl sol` shows the Linux console after a `UseSOL` reset; keystrokes arrive; session survives 10 minutes idle |
| M3 | KVM in Rust: RFB handshake, Raw and ZRLE decode, `kvm-snapshot` PNG; then the Tauri viewer with mouse and keyboard; latency log | Interactive 1080p session usable for BIOS setup from the Mac; the numbers for section 6 are recorded in the repo |
| M4 | IDER: ATAPI emulation, boot flow, progress events, cleanup of boot settings afterwards | MS-02 boots a Talos or Ubuntu ISO served from the Mac end to end, and the next plain reset boots from disk again |
| M5 | Tauri polish: host list, keychain, pin dialog, error surfaces, `cargo tauri build` | A fresh clone builds a `.app` that manages the MS-02 without reading this doc |

If M3 latency is bad on the plain Channel, spend the time on lever 1 and 2 from section 6 before anything in M5.

## Security requirements

- One module tree owns sockets (`amt-core::net`). Nothing else may import `tokio::net`, `reqwest` or `rustls`; a grep check in CI enforces it.
- Hosts and TLS pins in `~/Library/Application Support/<app>/hosts.json`; passwords in the keychain via the `keyring` crate; nothing in localStorage.
- No updater, no telemetry, no analytics, no remote fonts or scripts. Webview CSP: `default-src 'self'; connect-src ipc: http://ipc.localhost; img-src 'self' blob: data:`.
- Logs never contain passwords, Digest HA1, nonces or pixel data. Log target hosts, opcodes and byte counts only.
- Release: `tauri-action` on GitHub Actions, SHA-256 sums and a build provenance attestation attached to the release. Unsigned `.app` for now (`xattr -dr com.apple.quarantine` to run it).

## Gotchas

- AMT 19 refuses non-TLS entirely; any `http://`, 16992 or 16994 code path is dead code and should not exist.
- Digest nonces go stale quickly; handle `stale=true` without surfacing an error.
- Enable the redirection listener and KVM over WS-Man before opening 16995, or the TCP connect succeeds and the handshake fails.
- If the box is not in admin control mode or consent is not None, the firmware demands an opt-in code; detect `OptInRequired` and show a clear message instead of hanging.
- AMT's HTTP server: no chunked encoding, few concurrent connections, keep-alive helps; a burst of parallel enumerates gets 503s. Serialize WS-Man calls.
- WKWebView swallows Cmd-key shortcuts; map Cmd to Ctrl in the KVM view and document that Cmd-Tab cannot be forwarded.
- The ZRLE inflater is per session, not per rectangle.
- IDER chunk sizes and the floppy/CD slot semantics come from the START\_IDER reply; do not hardcode.
- After an IDER boot, clear `UseIDER` and the boot order, or every later reset tries the CD again.
- The AMT certificate changes when the box is unprovisioned, reprovisioned or the ME is reflashed; the pin dialog must let the user accept a new fingerprint deliberately, never silently.
- Keep the KVM, SOL and IDER sessions on separate connections; AMT does not multiplex them on one socket.
