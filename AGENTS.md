# koblibs engineering conventions

Shared libraries for the Kobbler ecosystem: eight standalone Rust crates plus the
`@koblibs/ui` Svelte package at the repo root. Consumed by kobbler, kobload and koblem.

Machine-specific notes — your build box, your deployment steps, your local paths — belong in
`CLAUDE.local.md` / `AGENTS.local.md`, which are gitignored. Do not add them here.

## Scope

Keep the libraries project-agnostic. No domain types from kobbler, kobload or koblem, and no
assumptions about how those applications are deployed. If a type only makes sense to one
consumer, it belongs in that consumer.

## Layout

This repo is **not a Cargo workspace**. Each crate is standalone — run cargo from inside the
crate directory. `Cargo.lock` is not committed, per library convention.

## Style

- **Zero comments by default.** Names and types are the documentation. Comment only
  non-obvious *why*: a constraint, a workaround, an invariant, or a coupling that is not
  visible from the code in front of you. Crate-level `//!` docs that explain what a crate is
  for and why it makes the choices it does are the exception, and are wanted.
- No backwards-compatibility shims and no commented-out code. Delete what you replace.
- Always use latest stable dependency versions — check the current release, not memory. Pins
  carrying an explanatory comment (the `boko` fork in `kob-ebook`, for one) are deliberate.

## Testing

Behaviour changes are test-first: write the failing test, watch it fail for the right reason,
fix, watch it pass. Drive the crate's public API rather than private intermediates, and cover
every entry point into the logic being changed.

```bash
cd <crate> && cargo test
cd <crate> && cargo clippy --all-targets
```

## Downstream impact

Consumers depend on this repo as a path dependency on a sibling checkout (`../koblibs`, or
`KOBLIBS_DIR` as a Docker build context), so:

- A public API change breaks kobbler, kobload and koblem simultaneously. Flag it, and expect
  downstream `Cargo.lock` regeneration.

Ecosystem: kobbler (`../kobbler/`), kobload (`../kobload/`), koblem (`../koblem/`).
