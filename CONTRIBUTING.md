# Contributing to koblibs

PRs are welcome.

koblibs sits underneath kobbler, kobload and koblem, so a change here can break all three at
once. Keep the crates project-agnostic: no domain types from the consuming applications, no
assumptions about how they are deployed.

## Getting set up

You need **Rust 1.99 or newer** (edition 2024). Most crates need nothing else. `kob-ffmpeg`
and `kob-ffmpeg-sys` additionally need FFmpeg development headers, `pkg-config` and libclang
— see [README.md](README.md#system-prerequisites).

This repo is **not a Cargo workspace**. Each crate is standalone, so run cargo from inside
the crate you are changing.

## Build and test

```bash
cd <crate> && cargo check
cd <crate> && cargo test
cd <crate> && cargo clippy --all-targets
cd <crate> && cargo fmt
```

Across everything:

```bash
for c in kob-*/ koblem-client/ nats-jobs/; do (cd "$c" && cargo check && cargo test) || exit 1; done
```

`nats-jobs` integration paths want a running NATS and PostgreSQL; `docker-compose.nats.yml`
starts the former. `cargo check` and the unit tests need neither.

For the Svelte package, `npm install` is the whole setup — there is no build step, since it
is published as source and compiled by each consuming app.

## Bug fixes are test-first

A behaviour change — a bug fix, a logic change — starts with a failing test.

1. Write a test that reproduces the bug. Run it, watch it fail, and check it fails for the
   reason you expect.
2. Fix it.
3. Run the test again and watch it pass.

Prefer integration-level tests over unit tests: drive the public API of the crate rather than
asserting on a private intermediate. If the same bug is reachable through more than one entry
point, cover all of them.

Keep tests fast and lean — shared fixtures over repeated heavy setup, table-driven tests for
variations.

## House style

- **Zero comments by default.** Names and types are the documentation. Comment only
  non-obvious *why*: a constraint, a workaround, an invariant, or a coupling a reader cannot
  infer. Crate-level `//!` docs explaining what a crate is for and why it makes the choices it
  does are wanted — several existing ones are worth reading before you change those crates.
- **No compatibility shims or commented-out code.** Delete what you replace; git remembers.
- **Latest stable dependency versions.** When bumping one, read its changelog and update the
  usage rather than only the number.
- These are libraries, so `Cargo.lock` is not committed.

## Breaking changes

Downstream repos build against a sibling checkout of this repo, so a breaking change lands on
all of them the moment they pull it. If your PR changes a public API, say so explicitly in the description and
name the downstream call sites it affects. Consumers will need to regenerate their
`Cargo.lock`.

## Pull requests

- One concern per PR.
- Say what was broken and how you proved it is fixed.
- `cargo check`, `cargo test`, `cargo clippy` and `cargo fmt` clean on the crates you touched.

## Licence

koblibs is licensed under the **AGPL-3.0**. By contributing, you agree that your
contributions are licensed under the same terms. See [LICENSE](LICENSE).
