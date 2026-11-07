# 112: Format upgrade and migration framework, with downgrade gates

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage`, `pizzlydb-graph`, `pizzlydb-incremental`, `pizzlydb-engine`, `pizzlydb-cli` |
| Type | Feature, compatibility |

## Summary

Provide a framework for evolving persisted formats after 1.0:

- **Version detection at open.** Every persisted structure is checked: the storage format, the graph logical format, the commit-log format, the plan and IR formats, the subscription records, and the token format. When an upgrade is needed and was not requested, the engine refuses to open with `UPGRADE_REQUIRED`.
- **In-place upgrade steps** for compatible changes. Each step is idempotent and crash-safe, and a pre-upgrade backup or checkpoint copy is mandatory.
- **Major upgrades** use export and import ([111](111-logical-export-import.md)).
- **Plan recompilation** happens on IR or compiler changes, as a plan-revision bump with documented subscriber impact.
- **Downgrades** are explicitly refused once an upgrade has committed.

## Motivation

Guide §12.1 lists version upgrade and downgrade gates as an operational test layer, and guide §15 requires an on-disk and protocol compatibility policy. Without a framework, the first post-1.0 format change would force either data loss or ad-hoc migration code.

## Current State

- Version fields exist in the superblock, WAL, pages, and some graph records. `UnsupportedVersion` is returned on a mismatch.
- The policy and the fixtures come from [024](024-storage-format-versioning-and-golden-fixtures.md).
- No upgrade path exists.

## Scope

### In scope

- **Version inventory.** A single `FormatVersions` struct persisted in the system key space and reported by `inspect`.
- **Upgrade registry.** An ordered list of `(from, to, step)`. Each step runs in bounded chunks and records its progress durably, so a crash resumes the step.
- `pizzlydb upgrade --data-dir ... [--dry-run]`, which requires a verified backup, or creates one first if configured.
- **Downgrade gate.** After an upgrade, a feature flag marks the files so that older binaries refuse them, using the [024](024-storage-format-versioning-and-golden-fixtures.md) incompatible-flag mechanism.
- **Plan handling.** On compiler or IR version changes, plans are recompiled. If compilation succeeds and the semantics are unchanged, the revision is kept. Otherwise the revision is bumped and subscribers receive `PLAN_INVALIDATED`.
- **CI.** For every historical golden fixture, open it, upgrade it to the current format, run `verify_full`, and check the content.

### Out of scope

- Rolling upgrades of a replicated cluster ([901](901-replication-and-high-availability.md)).

## Dependencies

- [024](024-storage-format-versioning-and-golden-fixtures.md): the versioning policy and fixtures.
- [111](111-logical-export-import.md): the major-upgrade path.

## Acceptance Criteria

- [ ] A simulated minor format change, introduced as a test-only step, upgrades the fixtures. A crash during the upgrade resumes correctly (crash simulation).
- [ ] An older binary opening an upgraded file fails with a clear error (test using a version-pinned test build).
- [ ] The upgrade procedure is documented in `docs/operations/upgrade.md`, and every release runs the fixture-upgrade CI job.

## Testing and Validation

- Fixture tests, crash simulation, and CI jobs.

## References

- [Guide §12.1](../architecture/implementation-guide.md#121-testing-layers), [§15](../architecture/implementation-guide.md#15-definition-of-done-for-pizzlydb-core)
