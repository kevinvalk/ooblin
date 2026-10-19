# Design

Calm, native-feeling, one look everywhere. Tailwind's own scale, our colors from `src/style.css`, no one-off sizes.

## Type

Everything is `text-sm` (14px) unless it's a title. Sentence case, never uppercase.

| What                       | Classes                              |
| -------------------------- | ------------------------------------ |
| Page title (the device)    | `text-xl font-semibold`              |
| Section title (`h2`)       | `text-base font-semibold`, sticky    |
| Card title, one of several | `h3`: `font-medium text-muted`       |
| Label, table header        | `text-muted` (headers `font-medium`) |
| Value                      | default, selectable, `wrap-anywhere` |
| Address, identifier        | `font-mono`, values and fields       |

Addresses and identifiers are monospace wherever they're a value or typed in: IP and MAC addresses, masks, GUIDs and
fingerprints (those `text-xs`, they're long), so they're easy to read and compare. Names and sentences aren't, the
sidebar's device names and the footer included. MAC addresses are written `12:07:14:5F:37:F1`.

## Layout

- A page is sections 48px apart (`gap-12`), at most `max-w-5xl` wide, 32px from the window's edges (`px-8`).
- A section is a title and one card, or a grid of titled cards when there are several of a thing (processors, memory,
  IPv4 and IPv6). Cards that belong to all of them (a port's link) span the grid.
- A card is `<dl class="card">` with a `<div><dt>…</dt><dd>…</dd></div>` per row: `rounded-xl`, hairline border and
  dividers, rows `px-4 py-2.5`. Headings, cards and tables are styled once in `src/style.css`.
- Tables span the page, their header stays in view.

## States

- Values come from Rust as data, the page says them in words right where it shows them: `{mhz && `${mhz} MHz`}`,
  Yes or No, times with `toLocaleString()` in the reader's own zone.
- Loading: the page itself, `aria-busy`, with gray bars where values go. Left out by the device: a dash. Both are CSS on
  empty `dt`, `dd`, `td` and `h3`, so markup just writes `{value}`. Not there at all (no battery): say so in words.
- Waiting on the device after a click: a spinning `LoaderCircle` where the click was (button, sidebar entry).
- The footer says what actions did for as long as it takes to read, and while the pointer is on it so it can be
  selected, what's true now (a device not answering) for as long as it is, and on its right how fast the disk image in
  the drive is read, while it is and a minute after.
- Errors: in the page with a way to try again, in the footer (in red) for actions and saving. A device that stops
  answering for two minutes is let go of, and the form says so, until then it's retried.

## Controls

- Buttons are `btn` with `btn-primary` (the one main action), `btn-secondary` or `btn-danger`, 32px high.
- Icons are 16px Lucide icons, icon-only buttons are `btn-icon` and have a `title`.
- Explanations go in a native `title` tooltip, not in the layout.
- Settings that can change are a `<form class="card">` of `label` and field rows (checkboxes, `.field` inputs and
  selects), the settings being the fields' defaults. A switch or choice of its own saves as it changes. Fields that go
  together (a name, an address) save together, from a ✓ and ✕ at the right of their card title, there all along so
  it's clear what saves this way and enabled once something changed. Enter saves and Escape cancels. Either way the
  footer says how it went, and the form then shows what the device took.
- Text fields for names and addresses don't capitalize or spell check (`autocapitalize="off" spellcheck="false"`).
- Anything that can lose work asks first, in a `<dialog class="dialog">`, as do small forms (scheduling a wake-up).
