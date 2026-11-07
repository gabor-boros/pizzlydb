# ADR-001: Rust workspace, edition 2024, forbidden unsafe code, and crate dependency direction

- Status: Accepted
- Date: 2026-11-07
- Owner issue: [001](../roadmap/001-design-record-and-baseline-adrs.md)
- Guide: [§1.4 design principles](../architecture/implementation-guide.md#14-design-principles), [§11 Rust implementation structure](../architecture/implementation-guide.md#11-rust-implementation-structure), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
- Supersedes: none

## Context

The guide requires a small safe Rust core (§1.4, principle 9) with an acyclic set of crates whose dependencies follow the system's layering (§11.1).

This ADR records the workspace shape, the unsafe-code policy, and the dependency direction that the repository already uses. It freezes them so that later changes need a superseding ADR.

## Decision

1. **One Cargo workspace.** The root `Cargo.toml` declares `members = ["crates/*"]` and `resolver = "3"`. Every crate inherits `edition = "2024"`, `rust-version = "1.88"`, the version, and the license from `[workspace.package]`.
2. **No unsafe code in workspace crates.** `[workspace.lints.rust]` sets `unsafe_code = "forbid"`, and every crate opts in with `[lints] workspace = true`. Because the level is `forbid`, a crate cannot re-enable unsafe code with `#[allow(unsafe_code)]`.
3. **Warnings fail CI.** `[workspace.lints.clippy]` sets `all = "warn"`, and CI runs `cargo --locked clippy --workspace --all-targets -- -D warnings`.
4. **Dependencies point down the layers.** A crate may depend only on the workspace crates listed for it below. Adding an edge needs an amendment to this ADR.

   | Crate | Workspace dependencies |
   | --- | --- |
   | `pizzlydb-core` | none |
   | `pizzlydb-storage` | none |
   | `pizzlydb-txn` | `core`, `storage` |
   | `pizzlydb-graph` | `core`, `storage`, `txn` |
   | `pizzlydb-query` | `core`, `graph` |
   | `pizzlydb-incremental` | `core`, `query`, `txn` |
   | `pizzlydb-subscriptions` | `core`, `incremental` |
   | `pizzlydb-auth` | `core` |
   | `pizzlydb-protocol` | `core` |
   | `pizzlydb-sdk` | `core`, `protocol` |
   | `pizzlydb-cli` | `sdk` |
   | `pizzlydb-testkit` | `core` |
   | `pizzlydb-server` | `auth`, `graph`, `incremental`, `protocol`, `query`, `storage`, `subscriptions`, `txn` |

   `pizzlydb-storage` stores ordered bytes and does not name graph types. `pizzlydb-graph` reaches storage only through the `StorageEngine` boundary ([ADR-003](0003-page-engine-and-ordered-keys.md)). `pizzlydb-server` is the composition root.

## Consequences

- Memory-mapped I/O (`mmap`) is unavailable to workspace crates, because the standard library offers no safe mapping API. This matches the page-engine design, in which all I/O goes through the VFS ([ADR-003](0003-page-engine-and-ordered-keys.md)).
- The `forbid` level covers workspace code only. Third-party crates such as `crc32c` may contain unsafe code. `cargo deny` and `cargo audit` in CI govern them.
- A crate that needs unsafe code, such as a future C ABI ([905](../roadmap/905-embedded-c-api.md)), requires a superseding ADR that isolates it in its own crate and states its safety invariants, tests, and reviewers (guide §11.2).
- A new crate, such as the `pizzlydb-engine` composition root proposed by [071](../roadmap/071-embedded-engine-composition.md), is added through an amendment to the table in decision 4.
- [005](../roadmap/005-benchmark-harness-and-baselines.md) pins `rust-toolchain.toml` to channel `1.98.1`. The workspace MSRV stays `1.88`. This ADR does not choose later compiler upgrades; Renovate proposes them.

## Alternatives considered

- **`deny` instead of `forbid`.** A crate could then allow unsafe code locally without review. Rejected, because the exception must be visible at the workspace level.
- **One crate with modules.** Module privacy does not stop a lower layer from importing a higher one. Separate crates make the compiler reject those edges.

## Verification

From the repository root, list the workspace edges and compare them with decision 4:

```shell
cargo metadata --format-version 1 --no-deps --locked |
  jq -r '.packages[] | .name as $n | .dependencies[]
    | select(.name | startswith("pizzlydb-")) | "\($n) -> \(.name)"'
```

Confirm that every crate inherits the workspace lints. The command prints nothing when all crates do:

```shell
for manifest in crates/*/Cargo.toml; do
  grep -q '^workspace = true' "$manifest" || printf 'missing lints: %s\n' "$manifest"
done
```

CI runs Clippy with `-D warnings`, and any `unsafe` block fails compilation.

## References

- [Rust lint levels: `forbid`](https://doc.rust-lang.org/rustc/lints/levels.html#forbid)
- [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)
