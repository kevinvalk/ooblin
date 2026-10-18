# AGENTS.md

A desktop Intel AMT console for Linux, macOS and Windows: a Tauri v2 app (`crates/amt-console`) with a Vite +
TypeScript UI (`ui/`, no framework) over a library without Tauri (`crates/amt-core`).

## What matters here

- Elegance, simplicity, no bloat, in the code and in the developer experience. Less is more, this file included.
- Tauri drives everything (`cargo tauri dev`/`build` start Vite themselves), few moving parts.
- Cross-platform from day one, nothing that only works on one OS.
- No abstraction, helper, knob or config until a second real use needs it.
- Use a solid, well-known crate over hand-rolling or vendoring the same thing.
- Comments only where a reader would otherwise get it wrong: terse, "we" voice, lines up to ~120 chars.
- Not sure how the protocol, firmware or a library behaves? Ask, don't guess.
- Kevin reviews all code, edits files while you work (treat unexpected changes as his) and does the commits.

## The handover is a guideline, not a spec

`AMT Console PoC Handover.md` was written up front and has not been checked or given an elegance pass. Use it for
protocol facts, reference code and milestones, but where it disagrees with this file or the code, those win. We already
diverged: no `amtctl` CLI and no `.env` (the app's connect screen is the way in), our own UI instead of the donor UIs,
Vite as in Tauri's own guide (plus `withGlobalTauri` for the MCP bridge), devices and pins in `hosts.json` in the app's
config dir, our own commands aren't capability-gated, and power state 8 is a hard off (the handover says soft).

## Working on it

- See `README.md` to run it. `cargo test` runs the library tests against a mock AMT, `npm run build --prefix ui`
  typechecks the UI.
- Only `crates/amt-core/src/net/` may touch the network, `cargo test` enforces it.
- Only Kevin can test against the real device (MS-02, AMT 19), we verify everything else ourselves.

## Verifying a change

We check our own work in the running app, against the mock AMT, without bothering Kevin:

1. `cargo verify` builds the debug app (hidden window, MCP bridge on localhost, and its own identifier, so its own
   devices and keychain entries, never Kevin's), `cargo build -p mock-amt` the mock.
2. Start `target/debug/mock-amt run go-wsman-messages` (or a real device's fixture, if there is one) and
   `target/debug/amt-console 127.0.0.1 --password 'P@ssw0rd'` as background tasks.
   Run the binaries themselves, not `cargo run`, so stopping the task stops the process. For a second session, connect
   to the same mock as `localhost`.
3. Open a driver session with `npx -y -p @hypothesi/tauri-mcp-cli@0.13 tauri-mcp driver-session start --port 9223`,
   retrying at most 20 times a second apart. Still nothing? Read the background tasks' output, never wait longer.
4. Drive the app (or use the `tauri` MCP server from `.mcp.json`): `webview-wait-for`, `webview-screenshot`,
   `webview-interact`, `webview-keyboard`, `webview-execute-js`, `read-logs --source console`. Wait for elements, never
   sleep. Look at every screen you touched and check the console for errors.
5. Always clean up: stop both background tasks, run `tauri-mcp daemon stop`, and check nothing listens on 9223,
   16993 or 16995 anymore.

The verify build runs without CSP, so CSP mistakes only show up in `cargo tauri dev`.
