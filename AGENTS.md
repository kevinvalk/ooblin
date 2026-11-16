# AGENTS.md

Ooblin, a desktop console for Intel AMT, Supermicro BMCs and any BMC over IPMI or Redfish on Linux, macOS and Windows: a
library that knows them all (`crates/ooblin-core`, each flavor in its own folder or file), a Tauri v2 app over it
(`crates/ooblin`) and a Svelte 5 and Tailwind UI (`ui/`, see `ui/DESIGN.md`). The library hands over data, the UI puts
it in words.

## Working here

- Write code the way the `code-taste` skill says, and read it before the first line.
- Run the `elegance` skill (`.claude/skills/elegance/SKILL.md`), at least three rounds, on every change before calling
  it done, and whenever the user asks for elegance. Where it disagrees with `code-taste`, `code-taste` wins.
- Tauri drives everything, `cargo tauri dev` and `build` start Vite. Cross-platform from day one.
- `cargo test` runs the library against the mock (`crates/mock`) and writes the UI's types to `ui/src/bindings` (ts-rs,
  committed: ooblin-core's by `#[ts(export)]`, the app's own by its `bindings` test), `npm run build --prefix ui`
  typechecks the UI. Only `crates/ooblin-core/src/net/` may touch the network, tests enforce it.
- `anyhow` and `?` outside the library. Spec and integration tests (fixtures, the running app) over unit tests.
- What a standard does (IPMI, Redfish) is written once, in the UI as in the library, and a vendor's pages take it from
  there as the library's `supermicro` does from its `ipmi` and `redfish`.
- Not sure how the protocol, firmware or a library behaves? Ask, don't guess. Where sources disagree, trust the MS-02's
  behavior, then Intel's Device Management Toolkit (console, ui-toolkit, go-wsman-messages), then MeshCommander, then
  Intel's AMT Implementation and Reference Guide.
- The user reviews all code, edits files while you work (unexpected changes are theirs), does the commits and is the
  only one to test against the real device (MS-02, AMT 19).

## Verifying a change

We check our own work in the running app against the mock, every command with a time limit
(`perl -e 'alarm 120; exec @ARGV' <command>`): a hang costs the user far more than a retry.

1. `cargo verify` builds the debug app (hidden window, MCP bridge on localhost, its own identifier so its own devices
   and keychain, never the user's, and no dialogs: it picks the file `OOBLIN_PICK` names and saves to `OOBLIN_SAVE`),
   `cargo build -p mock` the mock.
2. Run `target/debug/mock run x10` (a BMC on 16443, its IPMI on UDP 623, so first), `target/debug/mock run ms-02`
   (`go-wsman-messages` for Intel's samples) and `TAURI_MCP_NO_FOREGROUND=1 target/debug/ooblin 127.0.0.1 --password
'P@ssw0rd'` as background tasks, the binaries themselves so stopping a task stops it. The variable keeps screenshots
   from raising the window on macOS. `ooblin 127.0.0.1:16443 --password 'P@ssw0rd'` hands the BMC to the running app,
   `ooblin localhost` a second connection to the AMT mock. Discovery shows whichever mock got UDP 623 first. A serial
   console open in the hidden window can stall the bridge (xterm never paints there): close it before driving on.
3. `npx -y -p @hypothesi/tauri-mcp-cli@0.13 tauri-mcp driver-session start --port 9223`, at most 20 tries a second
   apart, then read the tasks' output instead.
4. Drive it (`webview-execute-js`, `-interact`, `-screenshot`, `-wait-for`, `read-logs --source console`, or the `tauri`
   MCP server): wait for selectors, never sleep or wait for text (the hidden window never paints, nor runs
   `requestAnimationFrame`, so a terminal's text stays unseen). Look at every screen you touched and check the console.
5. Clean up: stop the tasks, `tauri-mcp daemon stop`, and nothing may listen on 9223, 16443, 16993 or 16995.

The verify build runs without CSP, so CSP mistakes only show up in `cargo tauri dev`.
