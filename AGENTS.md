# AGENTS.md

A desktop Intel AMT console: a Tauri v2 app (`crates/amt-console`, UI in `ui/`) over a library without Tauri
(`crates/amt-core`).

## What matters here

- Elegance, simplicity, no bloat, in the code and in the developer experience. Less is more, this file included.
- No abstraction, helper, knob or config until a second real use needs it.
- Use a solid, well-known crate over hand-rolling or vendoring the same thing.
- Comments only where a reader would otherwise get it wrong: terse, "we" voice, lines up to ~120 chars.
- Not sure how the protocol, firmware or a library behaves? Ask, don't guess.
- Kevin reviews all code, edits files while you work (treat unexpected changes as his) and does the commits.

## The handover is a guideline, not a spec

`AMT Console PoC Handover.md` was written up front and has not been checked or given an elegance pass. Use it for
protocol facts, reference code and milestones, but where it disagrees with this file or the code, those win. We already
diverged: no `amtctl` CLI and no `.env` (the app's connect screen is the way in), our own UI instead of the donor UIs,
and pins in the app's config dir.

## Working on it

- `npm run dev` runs the app, `npm run build` bundles it, `cargo test` runs the library tests against a mock AMT.
- Only `crates/amt-core/src/net.rs` may touch the network, `cargo test` enforces it.
- Only Kevin can test against the real device (MS-02, AMT 19). Never block on the GUI: run `npm run dev` in the
  background and read its log.
