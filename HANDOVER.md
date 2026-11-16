# Ooblin root application improvement plan

## Decision and purpose

Continue the root Tauri and Svelte application as the maintained Ooblin implementation. Use the other three
implementations as sources of specific improvements and regression scenarios. Keep one application and one
`ooblin-core` implementation moving forward.

The destination is a small, dependable desktop AMT console: a library with explicit protocol guarantees, a Rust
application that owns sessions and resources, and a Svelte UI that owns presentation and editing. Preserve the
root's working KVM delta rendering, xterm terminal, explicit settings operations, native dialogs, and packaging
foundation. Improve correctness and diagnosis before adding features or reorganizing working code.

This is an implementation handover, not authorization to implement every item in one change. Each work package
below should leave a working application and include evidence for its behavior. The user reviews changes and
does commits. Only the user tests against the real MS-02.

## Context for the next maintainer

The comparison covered these working directories, including uncommitted files:

| Name used here | Directory relative to this repository |
| --- | --- |
| Root | `.` |
| New Tauri | `~/Development/worktrees/vivid-bolt/tauri-intel-amt-kvm` |
| Iced | `~/Development/worktrees/steel-isle/tauri-intel-amt-kvm` |
| egui | `~/Development/worktrees/whole-quartz/tauri-intel-amt-kvm` |

The `~` in these paths is a **literal directory inside this repository**, not the home directory.
Quote paths when using them in shell commands. Donor paths in this document require those worktrees to remain
available. Before removing them, preserve useful source references through the user's normal version control
workflow.

Read [AGENTS.md](AGENTS.md), [the UI design](ui/DESIGN.md), and
[the elegance skill](.claude/skills/elegance/SKILL.md) before implementation. Existing modifications belong to the
user. Do not reset them or overwrite them with a fork.

The earlier review ran protocol and mock integration tests for all four versions, native application tests for
Iced and egui, both Tauri frontend builds, and both Tauri verify builds. It inspected mock-driven Tauri screens
and native test screenshots. Those results are historical evidence, not a substitute for testing subsequent
changes.

Real firmware behavior, Windows and Linux execution, screen reader behavior, signed distribution, and sustained
performance were not verified in that review. Concurrency findings below distinguish visible source behavior
from risks needing a deterministic reproduction. Use the mock to establish ordering failures; ask the user for
firmware evidence where the correct protocol behavior is uncertain.

## What each version contributes

| Source | Adopt or preserve | Avoid importing wholesale |
| --- | --- | --- |
| Root | Explicit `Change` requests; exclusive client access across complete operations; request identity for page reads; binary KVM deltas; xterm; ordinary HTML forms and dialogs | Host-only session addressing; concurrent disk snapshots; swallowed serving errors; frontend enforcement of transport close ordering |
| New Tauri | Explicit resource lifetime ownership; observable redirection status; a deliberate distinction between an open session and the view showing it | A serializable whole-application Rust model; backend page caches and presentation state; whole-settings diff submission; generic dotted field paths; retry machinery embedded throughout the core API |
| Iced | Clear named messages and effects; task lifetime awareness; tests exercising real tasks and subscriptions | Implicit reopen on `next()` as the default core contract; full-frame RGBA copies; replacing xterm with a small custom terminal; results identified only by current form or host |
| egui | Session identity checks; serialization across enumeration and pulls; redirection ownership guards; readback after a failed settings change; broad workflow tests; typed changed rectangles | Its entire UI update mechanism; custom terminal and typography infrastructure; assumptions that its page-loading and confirmation behavior are already correct |

The important synthesis is **strong resource and protocol ownership with small presentation state**.
No fork is a correctness oracle. For example, egui's device generation guard is valuable, while its page read
guard and reachability confirmation handling still need their own scrutiny.

## Target ownership

| Responsibility | Owner | Contract |
| --- | --- | --- |
| TLS, pin verification, authentication, WS-Man, redirection packets | `ooblin-core` | Typed outcomes; no UI or Tauri dependency |
| Enumeration ordering and protocol read/write semantics | `ooblin-core`, with the existing application operation lock preserved | Callers cannot accidentally corrupt a live protocol transaction |
| Device connection generation, active resource slots, shutdown | Rust application | Old work cannot publish into or close a replacement session |
| Saved devices, credential-store access, local file selection and access | Rust application | One persistence owner; no secret returned to the webview |
| Reconnect scheduling | Initially the existing Svelte loop; move into a small Rust application owner when its lifecycle is proven | One scheduler per resource, cancellation tied to explicit ownership |
| Power polling | Initially Svelte | Preserve behavior; move only for a demonstrated lifetime requirement |
| Page data and refresh indicators | Svelte | Latest request wins within the current session |
| Settings drafts and confirmation dialogs | Svelte | Unsaved edits remain separate from observed device state |
| Protocol capability and validation | `ooblin-core` | Meaningful unsupported, invalid, and consent outcomes |
| Wording, units, locale formatting, layout, focus | Svelte | Follow `ui/DESIGN.md` |
| KVM decoding and remote key encoding | `ooblin-core` | Ordered input, changed rectangles, bounded work |
| KVM canvas and screen scaling | Svelte | Paint deltas without reconstructing the entire application model |
| SOL transport | `ooblin-core` and Rust session owner | Ordered bytes, independent receive and transmit, explicit termination |
| Terminal emulation, selection, scrollback and clipboard interaction | xterm and Svelte | Preserve terminal behavior already supplied by xterm |
| IDER protocol and image reads | `ooblin-core` | Serving failure remains observable |
| IDER task and selected image lifetime | Rust application | Explicit mount identity and cancellation |
| IDER progress visualization | Svelte | Display byte ranges or a small bounded progress summary |

Backend ownership does not require backend ownership of everything the UI displays. A Rust session can own a
KVM handle without owning the current tab, form defaults, HTML loading state, or cached hardware page.

### Keep the command boundary small

Continue using request/response commands for page reads and mutations, binary responses for KVM and SOL, and
narrow channels for ongoing activity. Add structured resource status only where an actual ongoing task needs it.

Do not introduce an application-wide event bus, a generic store synchronization protocol, or a new command
framework. `ts-rs` already supplies shared data types. A small typed invoke helper is worthwhile only where it
removes repeated, demonstrably unsafe command argument declarations; it should not recreate Tauri.

## AMT core contracts

### 1. Preserve complete-operation exclusivity

**Observed:** root [client.rs](crates/ooblin-core/src/amt/client.rs) uses `&mut self`. The application wraps each client
in `Arc<Mutex<Client>>` and holds that lock across page reads and settings mutations plus successful readback.
This gives a simple ordering guarantee across all WS-Man requests in each operation.

The egui client uses a shared internal mutex and holds it across an entire enumeration, including every pull.
The Iced and new Tauri clients use shared access with narrower request locking. Their convenience does not
establish equivalent transaction isolation.

**Decision:** retain the root's mutable client and outer operation lock for the first implementation stages.
It already prevents important interleavings. Do not convert every method to `&self` and add atomics merely to
make command code shorter.

Required guarantees:

- An enumeration and all of its pulls form one serialized operation on a client.
- A read-modify-write setter cannot lose updates to another setter using the same client.
- Mutation and immediate readback retain the existing application lock where practical.
- Different devices can proceed independently.
- Long-lived KVM, SOL and IDER traffic does not hold the WS-Man client lock.
- No synchronous session-map lock remains held across an `.await`.

If a later second consumer genuinely needs a cloneable client, evaluate the egui design, but preserve all of
these guarantees. A lock around each HTTP post alone is insufficient. Internal higher-level locking must avoid
recursively acquiring the same mutex through `get`, `put`, or `enumerate`.

Tests should make two enumerations overlap and give each several pages. Also overlap settings read-modify-write
operations. Make the mock expose the conflict instead of relying on timing or assuming a real device will.
The exact firmware concurrency restrictions still require evidence according to the repository's source order.

### 2. Make operation outcomes explicit

Keep the useful distinctions in [Error](crates/ooblin-core/src/lib.rs): authentication, pin mismatch, timeout,
transport, WS-Man fault, method return code, unsupported capability, disabled feature, consent and wrong code.
Do not flatten errors into strings before the application has made its policy decision.

Preserve the distinction between reads and mutations in `client.rs`:

- Safe reads currently retry once on transport failure.
- Timeouts currently do not trigger that immediate retry.
- Mutations are not automatically replayed after an ambiguous transport failure.

Do not assume every operation named “read” is safely replayable at every protocol step. Audit enumeration pulls
and any stateful query when changing retry behavior. Tests should simulate a response lost after the server
processed a request. A settings write or power action may have succeeded even when its response was lost.

Keep operational classification in data. UI wording belongs in Svelte, with useful protocol detail retained for
diagnostics. Add a new error variant only when it changes recovery, presentation, or diagnosis.

Audit broad fallback behavior separately: `enumerate` currently maps a general WS-Man fault to an empty list.
Some redirection capability detection also accepts broad errors as absence. Establish which faults mean
“unsupported” before narrowing them. An actual failure must not silently become “no hardware” or “no KVM,” but
older firmware compatibility must remain covered by fixtures.

### 3. Keep explicit settings intent

Root [settings.rs](crates/ooblin-core/src/amt/settings.rs) exposes specific setters and a `Change` enum.
Preserve the explicit intent: changing IPv4 sends an IPv4 change, adding an alarm sends an alarm operation.

New Tauri's `set_settings(were, wanted)` compares whole snapshots, while its UI builds a snapshot by walking
dotted form names through `Record<string, any>`. This shortens some form code but introduces a second schema,
stale baselines, and sequential partial-application behavior.

Do not adopt that path walker or whole-settings submission. Settings also contain read-only information,
capabilities and inventories; they should not become an implicit general-purpose write schema.

There is one worthwhile boundary cleanup: `Change::AddCertificate` currently contains a local filesystem path
and the core dispatcher reads it. Prefer moving the application command enum and its dispatch beside the Tauri
`change` command, retaining the existing typed core setter inputs. Read the certificate in the app and call
`Client::add_certificate(bytes, is_trusted_root)`. This follows egui's direct use of core setters.

Do this as one focused boundary change, after the settings outcome tests exist. Do not keep duplicate
application and library command enums with a conversion layer. If other real consumers need a domain-level
`Change`, retain only domain operations there and keep file selection/loading outside it.

Validate settings in Rust before sending them, even when HTML already validates the form. Preserve device
capabilities and optional values; do not turn unsupported fields into editable defaults. Use ordinary address
parsing where sufficient. Adopt the new Tauri discovery code's `ipnet` dependency only if its actual CIDR
handling replaces enough custom code and matches existing accepted input and error behavior.

### 4. Give redirection an explicit lifetime

Compare the current contracts before changing them:

| Version | Contract | Lesson |
| --- | --- | --- |
| Root | Open asynchronously, then `frame`/`read`, `input`/`write`, and `close` | Explicit and easy to bridge to IPC; close ordering needs stronger ownership |
| Iced | Obtain a handle, `next()` opens or reopens; synchronous `send()` queues input | Pleasant subscriptions, but opening and retry effects are implicit and queue errors need attention |
| egui | Open, then `run()` consumes ordered inputs and produces updates until termination | A clear task lifetime and explicit terminal result |
| New Tauri | `Kept<T>` maintains redirection and publishes `Link` state | Useful lifecycle idea; generic retry and UI status machinery need not live in the core |

**Decision:** keep explicit opening and termination. Keep retry scheduling in the application layer.
Do not make a core `read` silently reopen, ask for consent, or retry indefinitely.

The core should guarantee exclusive ownership of a redirection kind for handles created through the same
client lineage. Borrow egui's ownership-guard concept where it makes this guarantee enforceable. This cannot
prevent a different application or a separately constructed client from contacting the same physical device;
document that scope.

An ownership guard must cover the active resource and its close sequence. Dropping a Rust value cannot perform
an arbitrary asynchronous goodbye. Define what explicit close guarantees, what cancellation does, and how the
next opener waits. In particular, do not wait for an ownership guard while holding a lock required by the
current owner's shutdown.

Keep one public operational style for each resource. Do not expose `next`, `run`, `keep`, and `read` as competing
ways to do the same thing. A `run` API is worth adopting only if a concrete application task becomes simpler
and testable without losing backpressure or error delivery. The current pull interface can remain.

### 5. Separate decoded screen data from webview framing when it pays

Root `Kvm::frame()` returns a compact binary layout suited to the canvas. It efficiently preserves changed
rectangles. egui exposes a typed `Frame` containing `Tile` updates. Iced returns a complete RGBA framebuffer.

Preserve delta behavior. A 3840 × 2160 RGBA image is about 33 MB before additional copies; full-frame cloning
must not be the default cost of a small screen update.

A useful later cleanup is typed rectangle output inside the core, with the current binary encoder in the
Tauri adapter. This would let core tests assert geometry and pixels without depending on an IPC format.
Proceed only if it does not introduce avoidable allocations or a second framebuffer.

Keep a single encoder and a documented decoder contract: dimensions, integer endianness, rectangle bounds,
payload length, empty updates, resize behavior and full refresh. Validate malformed lengths and dimensions at
the appropriate boundary. Preserve RGB565/RGB332 decoding, ZRLE stream lifetime and corrective refresh behavior.
Those details earn their complexity through protocol behavior.

### 6. Keep core data portable without speculative library work

The existing typed XML input and parsed output approach earns its place. Prefer small, named domain structures
to raw string maps at the application boundary. Keep low-level XML handling where protocol work actually needs it.

Review public exports in [lib.rs](crates/ooblin-core/src/lib.rs): `Connection`, raw redirection and encoding helpers
are exposed alongside the high-level client. Determine which are needed by the mock or integration tests before
reducing visibility. Do not create a new “public SDK” compatibility project solely for this desktop app.

The new Tauri `method!` macro may remove repeated `Method` implementations. Adopt it only if the generated
behavior stays obvious beside the XML types and existing wire fixture tests remain readable. Do not generalize
it into a protocol description language.

Keep `ts-rs` while it prevents type drift. Making TypeScript generation optional is a later library-consumer
concern, not a prerequisite for a clean architecture.

## Application correctness work

### A. Session and operation identity

**Observed:** root [main.rs](crates/ooblin/src/main.rs) stores sessions by host and updates them by host after
awaiting work. A delayed KVM open, mount, change or connect can outlive the session it started for.
The source makes stale completion possible; the exact failing schedules should become tests.

Borrow egui's `Device::to_app` check in
`~/Development/worktrees/whole-quartz/tauri-intel-amt-kvm/crates/ooblin/src/device.rs`.
It checks both host and a session ID before changing the app.

Implement three identities only where needed:

1. A connection-attempt identity exists before login completes. Cancellation, forget or a newer attempt
   invalidates it. A late successful login must not resurrect a forgotten device or replace a newer connection.
2. A session identity exists for each accepted connection. Commands and asynchronous completions target it,
   not merely its host. A stale command must fail or be ignored without affecting the replacement session.
3. A resource identity distinguishes successive KVM, SOL and IDER instances inside that session. A delayed close
   or progress event from resource A cannot close or describe resource B.

A counter or opaque token is enough. Do not build a generic identity framework. If counters cross JSON,
choose a representation whose equality survives JavaScript number limits.

At a mutation's completion, compare identity before changing shared state or saved metadata. A captured client
can still finish an already-issued network action after the user disconnects; the UI must not imply that
cancelling a future undoes a remote action.

Preserve root `Device.svelte::read`'s existing latest-request guard. Its unique blank object makes the latest
read win. A session generation does not replace a page request identity: two reads can overlap within the same
session.

Acceptance scenarios:

- Start login A, cancel it, complete login B, then let A finish.
- Disconnect and reconnect the same host while a settings read or KVM open is pending.
- Start two refreshes and complete them in reverse order.
- Mount image A, start B, then deliver A's final progress and termination.
- Begin closing KVM A, open B, then deliver A's delayed cleanup.
- Forget a host while login is awaiting the mock; no late completion silently restores it.

### B. Saved hosts and credential consistency

**Observed:** root `connect` opens `Hosts` before awaiting login.
[hosts.rs](crates/ooblin/src/hosts.rs) writes the whole map using `std::fs::write`.
Two concurrent successful logins can save different old snapshots and lose one update. Other read/modify/write
operations also need a consistent serialization boundary.

Introduce one application owner for saved-host mutations. Prefer a small managed `Hosts` state over repeated
disk snapshots; keep the existing file format unless migration is necessary. Capture the pin and credentials
needed for login, release storage access, perform network work, then apply a generation-checked update.

Requirements:

- Do not hold a persistence lock through login or another network await.
- Serialize read-modify-write updates and use a replace strategy that leaves an intact old or new file.
- Verify replacement behavior on Windows as well as Unix; a rename assumption is not a cross-platform test.
- Surface malformed storage without silently replacing it with an empty file.
- Keep the previous valid state on a failed write; report the failure.
- Treat keyring and JSON updates as two fallible operations. Define recovery when only one succeeds.
- Keep “connected successfully” distinct from “password could not be remembered.”
- Preserve app-identifier separation for real, verify and test credentials.
- Decide whether multiple app processes are supported. A process-local mutex does not serialize two processes;
  either enforce a single-instance policy or add suitable file coordination when that requirement is established.

Test simultaneous connects to two hosts, rename concurrent with connect, write failure, corrupt JSON,
credential-store failure, and forget racing a pending connection. Check that a changed pin still requires the
existing explicit trust flow.

### C. Settings outcomes and partial failure

**Observed:** root `change` uses `client.change(...).await?` before readback. Some setters contain multiple
requests. The device can change partially before an error, leaving the old settings displayed.
egui's `Device::save` attempts a readback after either mutation outcome; adopt that principle.

Use a small application result that preserves both the mutation result and readback result. It may be a
serializable struct containing two results or an explicit equivalent; choose the representation that is clearest
with the existing generated types. Avoid a large outcome hierarchy.

| Mutation | Readback | Required UI behavior |
| --- | --- | --- |
| Success | Success | Show accepted values and success |
| Failure | Success | Show actual values and the original error; some changes may have applied |
| Success | Failure | Say the request succeeded but current state could not be confirmed |
| Failure | Failure | Preserve both errors; mark old displayed values stale |

Readback is best effort and bounded by existing request timeouts. Network/reachability changes can deliberately
cut off the address currently in use. Do not interpret that as proof nothing changed, loop forever, or replay
the mutation automatically.

Draft handling:

- Keep each form's edits separate from the last observed settings.
- Do not modify observed settings before confirmation.
- Cancelling a confirmation sends no change and leaves the observed state intact.
- A refresh or save in one section must not silently erase another section's draft.
- On failure, retain useful edits and provide an explicit retry or reset path.
- Update capability-dependent controls and close disabled resources from confirmed state; handle uncertain
  state explicitly when readback failed.
- Keep name persistence failure distinct from the device accepting a rename.

Tests should include a failure after the first request in `set_remote_access` or `set_ipv6`, success followed
by failed readback, cancellation of reachability changes, and unrelated dirty forms during a save.

### D. Resource ownership and reconnect

Root [Redirection.svelte](ui/src/device/Redirection.svelte) maintains a module-level `closing` map so a new open
waits for the previous close. This is an important guarantee whose authoritative enforcement belongs in Rust.
The current command implementations close a slot, await opening, and then store by host; they need the identity
work above.

Proceed incrementally:

1. Enforce open/close ordering and stale-resource protection in Rust while retaining the current Svelte retry loop.
2. Test close during open, close during read, reconnect after failure, and session replacement.
3. Remove the frontend `closing` map once backend tests prove it redundant.
4. If a Rust reconnect owner simplifies lifetime handling, move that loop in a separate change and remove the
   old scheduler at the same time.

If moving reconnect into Rust, keep the owner in the **application**, with only transport invariants in the core.
Use a few explicit statuses such as opening, open, retrying with reason, awaiting consent, declined, and closed.
Keep wording, buttons and dialogs in Svelte. Normal closure must not look like failure.

Cancellation must wake retry waits and unblock pending operations. Authentication and pin failures should
lead to a useful user action rather than an endless generic reconnect loop. Disabled features should lead to
settings guidance. Transient device resets should retain bounded backoff.

Consent is shared per device session. Concurrent KVM, SOL and IDER requests must not open competing dialogs or
start conflicting consent requests. Preserve root's shared consent promise initially. Wrong code allows retry;
decline stops automatic prompting until explicit user action. Disconnect resolves waiting requests and stops
their work. Do not cache consent as permanent authorization.

#### Define the view lifetime before adding persistence

Preserve sessions across ordinary tab switches, as root does today. Do not accidentally turn “hidden” into
“closed.” Test that reconnect of a hidden KVM or SOL does not steal focus.

Full webview reload is a separate feature. Keeping sockets alive does not recover the canvas or xterm history.
For the initial design, treat a lost frontend as a resource-consumer loss: define a bounded teardown and clean
reopen behavior. If reload survival is later required, specify:

- how a new view attaches to an existing resource;
- how it receives a complete initial KVM image before deltas;
- whether SOL history is deliberately lost or retained in a bounded buffer;
- how old input producers and subscriptions are invalidated.

Do not claim reload recovery merely because the Rust handle survives.

### E. IDER status and completion

Root `ider_mount` deliberately ignores `serve`'s error and sends `None` on termination. This prevents the UI from
distinguishing normal closure from a failed image read or broken protocol session.

Keep file data out of the webview. Keep the existing protocol implementation and its boot behavior. Replace the
ambiguous completion notification with a small typed event carrying progress or a terminal outcome, scoped to
the mount identity. Eject and replacement should have explicit application semantics.

Preserve the distinction between total bytes transferred and unique image regions read. Repeated reads can
increase the former without increasing the latter. Keep visualization data bounded; do not send the entire
application model for a progress update.

Tests: eject during serving; replacement while old termination is pending; device-initiated closure; image
read failure; transport loss; boot-to-image after serving has ended; disabled IDER; cancellation during open.
Continue checking in Rust that boot-to-image has a usable mount.

### F. Discovery and connection health

Keep discovery results separate from saved, authenticated devices. A discovery response is not proof that
credentials work, that provisioning is complete, or that its certificate should be trusted.

Root discovery already has an owned task and stop command. Preserve that simple shape. Add a search identity
if the UI can receive queued results after cancellation or replacement. Stopping search A must not clear or
populate search B, and completion must distinguish an actual search error from deliberate cancellation.
Deduplicate results consistently while preserving the address the user actually connected through.

Test empty input, invalid CIDR, overlapping ranges, cancellation, two successive searches, duplicate responses,
and discovered devices that cannot subsequently authenticate. Keep verify discovery restricted to the mock.
Do not automatically scan real networks during development or tests.

Preserve the root's power behavior initially: periodic reads for connected devices, immediate refresh when
needed, one read in flight, unknown power during temporary unreachability, and the existing two-minute lost
connection threshold. Use monotonic elapsed time for backend health decisions if polling moves into Rust.

Separate these outcomes:

- A normal boot/reset temporarily interrupts access.
- The device remains unreachable beyond the threshold.
- Authentication or certificate trust changed.
- The response is malformed or an unsupported operation was requested.

New Tauri's polling catch-all clears its silent timer on non-unreachable errors without reporting them.
Do not adopt that behavior. Preserve an actionable error and avoid treating every failure as successful
evidence of health. Test recovery before the threshold, sustained loss, and a stale poll completing after
session replacement.

### G. Shutdown and IPC hygiene

Define teardown for explicit resource close, device disconnect, frontend loss and application exit. Resolve
pending consent, stop discovery and reconnect tasks, invalidate input producers, and release mounted files.
Attempt protocol close where supported with a bounded deadline; retain a forced-cleanup fallback.
Do not introduce unbounded shutdown waits or imply that aborting a task reverses an AMT mutation.

Check every new IPC type for serialization behavior: optional values, tagged errors, byte buffers, timestamps
and large integer counters. Keep secrets and runtime handles out of serialized values. Test the few boundary
shapes that could silently diverge between Rust and TypeScript; generated types alone do not validate runtime
payloads or command argument names.

Treat command arguments as untrusted inputs at the backend boundary. Apply session/resource identity checks
to reads and writes as well as cleanup. Scope channels to their intended consumer, and stop or detach work
deliberately when sending fails instead of retaining an orphaned task.

## UI and interaction work

### Preserve the working baseline

Follow the existing design: calm layout, semantic cards and tables, native HTML controls, consistent units,
selectable values, local time formatting, visible loading state and useful action feedback.

The app uses a native desktop webview with HTML controls. Its main interface is not AppKit, WinUI or GTK native
OS controls. This does not prevent good desktop behavior or accessibility; those depend on implementation
and platform testing.

Keep xterm. The Iced terminal's small parser view and limited interaction are not equivalent replacements.
The egui terminal adds more selection, paste and scrollback behavior, but maintaining those features manually
would duplicate work xterm already does.

### KVM and serial input

Review [Kvm.svelte](ui/src/device/Kvm.svelte) and [Sol.svelte](ui/src/device/amt/Sol.svelte) with these rules:

- Input goes only to the active, intentionally focused resource.
- Opening a dialog, switching device, hiding a view, losing window focus, pointer cancellation and disconnect
  cannot leave keys or mouse buttons held remotely.
- A reconnect does not replay stale typing or a queued destructive shortcut.
- Movement may be coalesced; key transitions, button transitions, wheel actions and SOL bytes preserve order.
- Slow transport has a bounded queue policy. Dropping arbitrary serial bytes is not acceptable backpressure.
- Input failure becomes observable without flooding the footer once per keystroke.
- A hidden resource reconnect never calls focus in a way that interrupts the visible form or console.
- Screen selection failure is handled and the selected value reflects confirmed state.
- Fullscreen state is reconciled when the window or user exits it, including Escape and tab changes.

Preserve physical key handling and intentional macOS modifier behavior until platform tests justify changes.
Test non-US keyboard layouts, modifier release, AltGr, paste, terminal application cursor mode and control keys.
Document the current KVM text-entry US-keyboard limitation where users choose that action.

Keep the SOL geometry of 80 × 25 unless firmware evidence supports another behavior. Resizing the local window
does not establish that remote firmware supports terminal-size negotiation.

### Accessibility

Audit actual semantics and keyboard interaction:

- Accessible names for every icon-only action, canvas, terminal and field; a tooltip alone is not an assumed
  substitute for a verified accessible name.
- Tab list/tabpanel relationships, selected state and keyboard movement.
- Focus placement and restoration for dialogs, including consent errors.
- A visible focus indicator on the KVM capture surface and a documented way to leave it.
- Status and error announcements that do not speak every frame or progress update.
- Text contrast, high contrast settings, reduced motion and scaling.
- xterm's screen reader support, explicitly evaluated with the shipped version and actual readers.

Test VoiceOver on macOS, NVDA on Windows and Orca on Linux. Include the terminal and settings dialogs, not only
the sidebar. The remote framebuffer itself has no local semantic accessibility tree; make the surrounding
controls accessible and retain SOL as a useful alternative where the remote environment supports it.

## Diagnosis

Add enough bounded operational history to answer “which operation failed, in which session, and what happened
next?” A transient footer alone cannot explain an intermittent reconnect or partial settings change.

Start with one structured logging mechanism already suitable for the Rust/Tauri application, plus explicit
frontend error reporting. Choose the implementation during that work package; avoid overlapping logging systems.

Useful fields: application version, platform, operation, session/resource identity, duration, outcome category,
retry attempt and relevant protocol method/return code. Correlate an IPC operation with its Rust work.

Exclude passwords, authorization headers, consent codes, terminal contents, KVM input text and unreviewed
protocol payloads. Hostnames, certificates and image paths can also be sensitive. Export only a bounded,
user-initiated diagnostic report with a clear account of what it contains.

Audit ignored errors individually: IDER serving, KVM input, SOL writes, screen selection, channel delivery,
polling, and persistence. A closed receiver during intentional teardown can be normal. An image read failure
or persistent write failure deserves a visible outcome. Avoid blanket replacement of every ignored result
with a dialog.

## Testing and delivery

### Test behavior at its cheapest reliable boundary

| Boundary | Essential coverage |
| --- | --- |
| Core fixture tests | XML shape, return codes, decoding, malformed/truncated packets, unsupported capabilities |
| Core plus mock | Enumeration isolation, retry ambiguity, redirection exclusivity, shutdown, input ordering, partial mutation |
| Application plus mock | Session replacement, stale completion, persistence races, resource lifetime, outcome mapping |
| Running Tauri UI | Confirmation, dirty forms, consent, focus, terminal interaction, recovery and actual rendering |
| Packaged application | CSP, permissions, credential store, file dialogs, webview behavior and install/upgrade |
| User with MS-02 | Firmware-specific behavior, reset recovery, real KVM/SOL/IDER, reachability changes |

Borrow **scenarios** from egui's `crates/ooblin/src/tests.rs` and Iced's `crates/ooblin/tests/app.rs`.
Do not import their UI harnesses. The existing root Tauri mock/MCP path should exercise the root application.

Extend the mock with explicit synchronization points and fault injection only for required cases. Tests should
control “request received,” “allow response,” and “close transport,” instead of depending on sleeps.
Use independent test ports and isolated storage and credentials.

The network-boundary source test remains useful, but it is not a proof of network isolation or runtime security.
Keep real sockets under `crates/ooblin-core/src/net/` as required by AGENTS.md.

### Verification for each implementation change

Use the exact current AGENTS.md workflow, including time limits. At minimum:

1. Relevant core and application tests, then required workspace checks.
2. `cargo clippy --workspace --all-targets`.
3. `npm run build --prefix ui`.
4. Regenerate and review committed TypeScript bindings after contract changes.
5. `cargo verify`, build the mock, and inspect every affected screen and console log.
6. Run a production-CSP check as well; verify deliberately disables CSP.
7. Clean up the owned app/mock processes and MCP daemon, checking ports 9223, 16993 and 16995.
8. Complete at least three elegance rounds and report what each simplification preserved.

The hidden verify window does not run normal animation-frame rendering. A blank xterm screenshot there does
not prove serial failure or success. Add an appropriate visible-window or terminal-model check for terminal
behavior. Do not claim terminal rendering was verified from a protocol test alone.

For this handover itself, only documentation validation is needed; no application behavior is changed.

### Cross-platform release work

Add repeatable CI around the existing build before changing packaging tools. Pin a supported Rust toolchain and
Node version, use lockfiles, and make generated binding drift visible.

Start with compile, lint, mock tests and frontend checks on Windows, Linux and macOS. Add a packaged smoke test
where feasible. Choose supported OS versions and CPU architectures explicitly; do not promise targets merely
because a framework supports them.

| Platform | Verify in a real packaged build |
| --- | --- |
| Windows | WebView2 availability, chosen installer, signing, credential storage, file replacement, scaling, keyboard capture, fullscreen |
| Linux | Supported distribution baseline, WebKitGTK/GTK and keyring dependencies, X11/Wayland behavior, file dialogs, packaging format, scaling |
| macOS | Supported architecture builds, signing/notarization, Keychain prompts, WKWebView, VoiceOver, modifier handling, multi-monitor fullscreen |

Node builds the frontend; it need not become a shipped application runtime. Keep Tauri as the build coordinator.
Keep debug inspection tooling out of production, and verify the actual release capabilities and CSP.

Test upgrades with saved devices and credentials, installation on a clean machine, and failure when a credential
service is unavailable. Do not add an updater, telemetry system, or remote configuration service as incidental
scope. They create separate operational responsibilities.

## Ordered work packages

### Package 0 Establish the baseline

Inventory existing working-tree changes and identify the current root files. Capture behavior and selected
screens against the mock. Preserve the donor sources needed below. Add only the test seams required for the
next package.

**Done when:** current tests/builds are recorded, root behavior is reproducible, and unrelated user changes are
preserved. Do not start with a broad file reorganization.

### Package 1 Stop stale work and lost host updates

Implement connection/session/resource identity and one saved-host mutation owner. Add deterministic regression
tests for reconnect, forget, concurrent connects and late mount/open completion. Handle persistence failures
without confusing them with login failures.

**Done when:** old work cannot alter a replacement session or resurrect a forgotten host; concurrent saved-host
updates survive; write and keyring failures have tested outcomes.

### Package 2 Make settings and background failures truthful

Preserve mutation and readback results. Cover partial changes and dirty forms. Report IDER terminal errors and
scope progress to its mount. Add the first diagnostic events needed to understand these scenarios.

**Done when:** success, failure and uncertainty are distinct; the UI shows observed state or clearly stale state;
failed serving cannot look like a healthy mounted image.

### Package 3 Enforce resource lifetime in Rust

Preserve full-operation client locking. Enforce redirection open/close ordering and cancellation. Remove
frontend close serialization only after proving the backend contract. Decide with evidence whether moving the
retry loop into a small application owner simplifies the system.

**Done when:** rapid open/close, reset, declined consent, disconnect and same-host reconnect behave correctly;
one retry scheduler exists; hidden resources do not steal focus; shutdown does not deadlock. Discovery
cancellation and power polling also reject stale work and retain useful failure information.

### Package 4 Refine core boundaries

Move filesystem-bearing settings dispatch to the app without duplicating command enums. Consider typed KVM
rectangles and an adapter encoder. Narrow public transport exports if possible. Adopt small boilerplate or
CIDR simplifications only where their tests and dependency tradeoffs support them.

**Done when:** the core explains AMT behavior independently of the webview, while the root remains as simple to
call and no performance or behavior is lost. A cloneable-client rewrite is not required.

### Package 5 Finish interaction and accessibility

Fix focus, key release, bounded input handling, terminal behavior, dialog accessibility and status announcements.
Preserve the existing visual language and test light/dark themes, scaling and keyboard-only use.

**Done when:** representative workflows pass on actual platform webviews and screen readers, with any remaining
limitations recorded concretely.

### Package 6 Make releases routine

Complete the CI matrix, packaged smoke tests, signing/notarization procedure and clean-machine installation
checks. Record an intentionally small supported platform matrix.

**Done when:** one maintainer can reproduce a release and diagnose a failed build or runtime report without
reconstructing undocumented local setup.

CI build checks can begin alongside earlier packages. Do not delay obvious test automation until all UI work
is finished. Conversely, do not let release automation conceal unresolved correctness failures.

## Performance work requires measurements

Before changing frame transport or adding backend caches, measure:

- Idle CPU and memory with one and several connected devices.
- KVM input latency, decode time, payload size and paint time for quiet and busy screens.
- 1080p and, where available, 4K behavior.
- Concurrent KVM, SOL, IDER and settings/power reads.
- Memory after repeated open/close/reconnect cycles.
- Large hardware and log responses.
- Input and output queue growth under deliberately slow consumers.

Use the root as the baseline. New Tauri's whole-model serialization is a plausible scaling concern, not a
measured bottleneck from the comparison. Avoid introducing it and then building a patch protocol to compensate.
Keep independently changing high-frequency data out of whole-application snapshots.

Dropping KVM deltas is unsafe unless the next delivered update reconstructs the missed state. Coalescing must
produce a correct image, not just a shorter queue. Preserve ordering of resize and pixel updates.

## Elegance decisions

### Complexity that earns its place

- Device/session/resource identity, because asynchronous work can outlive its initiator.
- Exclusive protocol operations, because independent requests can participate in shared device state.
- Explicit partial-success outcomes, because AMT operations are not general database transactions.
- KVM delta decoding and corrective refresh, because bandwidth and firmware behavior matter.
- xterm, because terminal emulation and interaction are substantial features.
- Native credential storage and certificate trust handling.
- A realistic mock and deterministic failure scenarios.
- A small amount of task ownership and cancellation code.

### Complexity to avoid or remove

- A Rust mirror of page caches, form state, formatting and tab selection.
- Whole-model serialization for unrelated updates.
- Untyped dotted-path settings mutation.
- Multiple reconnect loops or competing stream APIs.
- A cloneable client whose apparent simplicity hides weaker ordering.
- Hand-maintained copies of generated cross-language types.
- Generic helpers used once, speculative traits, or splitting files only to reduce their line count.
- Custom terminal or widget infrastructure already supplied by the browser or xterm.
- New dependencies whose benefit is only a few superficial lines.

Review each package in three rounds:

1. **Ownership:** every value has an obvious owner; remove duplicated authority.
2. **Behavior:** cancellation, errors and stale work are explicit without duplicating state.
3. **Surface area:** remove scaffolding, redundant helpers, types, files and dependencies after tests prove the
   remaining design.

Count code and dependencies as useful signals, not as the definition of complexity. A dozen lines preventing
a late close from destroying a new session are cheaper than an intermittent production bug.

## Evidence that could change these decisions

Revisit the conservative client API if a real second consumer needs concurrent shared access and an alternative
proves equivalent transaction guarantees with fewer total mechanisms.

Move polling or retry ownership into Rust when tests demonstrate a lifecycle problem that cannot be resolved
cleanly at the existing boundary. Do not move page rendering state along with it.

Adopt a backend snapshot only for a concrete need such as multiple synchronized windows or deliberate reload
recovery, with identity, attachment, initial screen data, history limits and measured serialization costs.

Reconsider frame transport after measurements identify an actual bottleneck. Preserve terminal and input
behavior when comparing implementations; a lower memory number from missing functionality is not a win.

The root remains the preferred implementation because it combines a useful existing UI, a capable terminal,
efficient screen updates, and established distribution machinery with comparatively direct application flow.
The plan strengthens that foundation while borrowing the forks' best correctness and lifetime ideas.

## First continuation task

Start with Package 1, in a small reviewable slice:

1. Read the current root connection/session and saved-host paths.
2. Reproduce concurrent connect persistence loss and stale same-host completion using the mock.
3. Introduce the minimum connection/session identity and saved-host serialization needed to fix them.
4. Preserve current UI and protocol behavior.
5. Run the relevant tests and the required mock verification workflow.
6. Complete three elegance rounds and report the exact behavior changes, evidence and remaining work.

Do not merge a donor branch, rewrite the application model, or change client concurrency semantics as part of
that first slice.
