# 101: Configuration system and data-directory layout

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-engine` (or the server composition root), `pizzlydb-cli` |
| Type | Feature |

## Summary

Define a typed, versioned configuration schema covering storage, transactions, query limits, maintenance, retention, subscriptions, resources, and telemetry, and later network and security. The issue has four parts:

- **Loading:** configuration comes from a file (TOML), environment overrides, and CLI flags, with a documented precedence.
- **Validation:** every value is checked before startup, with clear error messages.
- **Data directory:** a specification of the layout (main file, WAL, lock file, temporary and spill files, backup staging, archive staging) and its permissions.
- **Reload:** a documented subset of settings can be reloaded at runtime.

## Motivation

Every Phase 1 issue added tunables: frame count, WAL limits, batch limits, checkpoint policy, transaction limits, maintenance budgets, and retention windows. Today they are constructor arguments. Operators need one validated, documented source of configuration. Guide §10.1 requires a configuration schema.

## Current State

- `Storage::open(vfs, path, frame_count, durability)` takes positional arguments. No config struct, file format, or validation exists.
- `crates/pizzlydb-cli/src/main.rs` is `fn main() {}`.

## Scope

### In scope

- A `Config` struct with nested sections, `serde`-deserializable, with `deny_unknown_fields` and a schema version field.
- Defaults documented inline, and a generated reference document (`docs/operations/configuration.md`) produced from the schema. A JSON Schema export lets editors validate files.
- Validation of ranges and cross-field constraints, for example that the buffer pool fits within the memory budget.
- Environment variable mapping (`PIZZLYDB_STORAGE__BUFFER_POOL_BYTES`), CLI overrides, and `config dump` with secrets redacted.
- A data-directory specification: file names, a permissions check (refuse to start on a world-writable directory), and a check that the directory is on a supported local file system (warn on NFS).
- A dynamic reload subset, such as limits, budgets, and log level, triggered by `SIGHUP` or the admin API. Every other setting requires a restart.

### Out of scope

- Secrets providers ([210](210-secrets-management.md)); the config only references them.

## Dependencies

- [071](071-embedded-engine-composition.md): the engine composition root consumes the config.

## Acceptance Criteria

- [ ] Every engine tunable is reachable through the config, and none are hard-coded (verified by audit).
- [ ] An invalid config fails at startup with an error that names the field and the constraint (table-driven tests).
- [ ] The reference document is generated, checked in, and verified to be current in CI.
- [ ] Reloading a dynamic field takes effect without a restart. Attempting to reload a static field is rejected with a message (test).

## Testing and Validation

- Unit tests for parsing, precedence, and validation, and an integration test of reload.

## References

- [Guide §10.1 deployment](../architecture/implementation-guide.md#101-deployment), [§10.4](../architecture/implementation-guide.md#104-resource-control)
