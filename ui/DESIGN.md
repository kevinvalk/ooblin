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
- Errors: in the page or form with a way to try again, in the footer for actions. A device that stops answering is let
  go of, and the form says so.

## Controls

- Buttons are `btn` with `btn-primary` (the one main action), `btn-secondary` or `btn-danger`, 32px high.
- Icons are 16px Lucide icons, icon-only buttons are `btn-icon` and have a `title`.
- Explanations go in a native `title` tooltip, not in the layout.
- Anything that can lose work asks first.
