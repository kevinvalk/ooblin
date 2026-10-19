# AGENTS.md

A desktop Intel AMT console for Linux, macOS and Windows: a library that knows AMT (`crates/amt-core`), a Tauri v2 app
over it (`crates/amt-console`) and a Svelte 5 and Tailwind UI (`ui/`, see `ui/DESIGN.md`). The library hands over data,
the UI puts it in words.

## Working here

- **Rule one, above all others: elegance.** Run the `elegance` skill (`.claude/skills/elegance/SKILL.md`), at least
  three rounds, on every change before calling it done, and whenever the user asks for elegance.
- Tauri drives everything, `cargo tauri dev` and `build` start Vite. Cross-platform from day one.
- `cargo test` runs the library against the mock AMT and writes the UI's types to `ui/src/bindings` (ts-rs, committed),
  `npm run build --prefix ui` typechecks the UI. Only `crates/amt-core/src/net/` may touch the network, tests enforce it.
- `anyhow` and `?` outside the library. Spec and integration tests (fixtures, the running app) over unit tests.
- Comments only where a reader would otherwise get it wrong: terse, "we" voice, lines up to ~120 chars.
- Not sure how the protocol, firmware or a library behaves? Ask, don't guess.
- `AMT Console PoC Handover.md` is an unchecked guideline: use it for protocol facts. Where it disagrees, this file and
  the code win.
- The user reviews all code, edits files while you work (unexpected changes are theirs), does the commits and is the
  only one to test against the real device (MS-02, AMT 19).

## Verifying a change

We check our own work in the running app against the mock AMT, every command with a time limit
(`perl -e 'alarm 120; exec @ARGV' <command>`): a hang costs the user far more than a retry.

1. `cargo verify` builds the debug app (hidden window, MCP bridge on localhost, its own identifier so its own devices
   and keychain, never the user's), `cargo build -p mock-amt` the mock.
2. Run `target/debug/mock-amt run ms-02` (`go-wsman-messages` for Intel's samples) and
   `TAURI_MCP_NO_FOREGROUND=1 target/debug/amt-console 127.0.0.1 --password 'P@ssw0rd'` as background tasks, the
   binaries themselves so stopping a task stops it. The variable keeps screenshots from raising the window on macOS. A
   second session connects to the same mock as `localhost`.
3. `npx -y -p @hypothesi/tauri-mcp-cli@0.13 tauri-mcp driver-session start --port 9223`, at most 20 tries a second
   apart, then read the tasks' output instead.
4. Drive it (`webview-execute-js`, `-interact`, `-screenshot`, `-wait-for`, `read-logs --source console`, or the `tauri`
   MCP server): wait for selectors, never sleep or wait for text (the hidden window never paints, nor runs
   `requestAnimationFrame`, so a terminal's text stays unseen). Look at every screen you touched and check the console.
5. Clean up: stop both tasks, `tauri-mcp daemon stop`, and nothing may listen on 9223, 16993 or 16995.

The verify build runs without CSP, so CSP mistakes only show up in `cargo tauri dev`.
