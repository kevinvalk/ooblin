---
name: elegance
description: This project's first rule, the same result with fewer lines, files, types, dependencies and moving parts. Run it, at least three rounds, on every change before calling it done, and whenever the user asks for elegance, an elegance pass, less is more or simplifying.
---

# Elegance

Elegant means pleasingly ingenious and simple: the same result with less. It beats every other rule in this project.

## What elegant is

- Less is more: fewer lines, files, types, dependencies and moving parts, in code, docs and developer experience. If
  something can go without losing quality or behavior, it wasn't elegant enough. Weigh what each piece buys, though.
- Every abstraction earns its place: no type, trait, helper, wrapper, knob or config until a second real use needs it.
  One that doesn't pay its way goes, one that does gets leaned into.
- Plain over clever: a `String` is enough until a type actually buys something.
- Together over sprawl: what changes together lives together, a type next to its only user, one file rather than
  three small ones. Split by meaning when a file holds unrelated things, never by size or kind.
- Each thing where its language says it best. A wash in size goes where the architecture wants it: AMT knowledge in
  the library, wording in the UI.
- Proper tools over strings: structured data (typed XML, serde) and well-known crates over templates and hand-rolling,
  the platform (HTML, CSS, the browser) before code of our own.
- Consistency is elegance: one way to do a thing, the same names, shapes and layout everywhere.

## The pass

Don't stop at the first thing that works.

1. **Scope** the whole codebase unless the user names a part, never just what you wrote last. Count it:
   `find crates ui/src -name '*.rs' -o -name '*.svelte' -o -name '*.ts' -o -name '*.css' | xargs wc -l | sort -n`,
   `cargo tree --depth 1`, `ui/package.json`.
2. **Question every piece** against the list above: file, type, trait, helper, dependency, command, config, doc line.
   Can something we already have do it (std, a crate we use, Svelte, CSS, the browser)? Is it a string where structure
   belongs? Would it be shorter on the other side of `invoke`? Does it sit where its readers would look for it?
3. **Research before deciding.** Read the source and docs of what we use (`~/.cargo/registry/src`, `ui/node_modules`),
   look for what exists (crates.io and npm: downloads, last release, maintainer), prototype in a scratch project
   outside the repo when unsure. Weigh at least two ways, know why the others lose.
4. **Cut**, biggest first: files and dependencies, then types and abstractions, then lines. Then back to 2 with the
   result: at least three rounds, and on until a whole round finds nothing.
5. **Prove** it: `cargo test`, `cargo clippy --workspace --all-targets`, `npm run build --prefix ui`, the verify loop
   on `ms-02`, and count again.
6. **Report** the counts before and after, what went, what stayed and why, what you weighed and why it lost.

Never trade away behavior, safety or a test of what we must not get wrong.
