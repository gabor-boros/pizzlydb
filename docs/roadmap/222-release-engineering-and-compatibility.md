# 222: Release engineering, versioning, and compatibility guarantees

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | M |
| Crates | CI, all published crates |
| Type | Release |

## Summary

Define and enforce the compatibility contract. It has four parts:

- **Semver policy** for the server, the protocol ([201](201-native-protocol-definition.md)), the SDK, the on-disk format ([024](024-storage-format-versioning-and-golden-fixtures.md)), the backup format, and the configuration.
- **Support windows,** including the supported upgrade paths ([112](112-format-upgrade-and-migration.md)).
- **Deprecation process.**
- **Release checklist:** gates, artifacts, signatures, SBOM, changelog, docs version, and an upgrade test.

An automated compatibility matrix runs in CI: old SDK against new server, new SDK against old server, the N-1 data directory opened by version N, and N-1 backups restored by version N.

## Motivation

Guide §8.1 requires compatibility tests for adjacent protocol versions, and guide §15 requires upgrade testing. release-please already manages versions and changelogs; what is missing is the *contract* and its enforcement.

## Current State

- `.github/workflows/release-please.yml` handles release PRs, tags, and Linux archive builds with attestations.
- There is no compatibility policy and no cross-version tests.

## Scope

### In scope

- **`docs/compatibility.md`.**
- **CI matrix jobs** that use the previous release artifacts, for SDK/server pairs, data directory upgrades, and backups. Golden fixtures come from [024](024-storage-format-versioning-and-golden-fixtures.md).
- **Release candidate process:** an RC tag, the chaos and soak gates ([220](220-chaos-soak-and-simulation-testing.md)), the SLO gate ([219](219-benchmark-suite-and-slos.md)), and sign-off.
- **`cargo-semver-checks`** for published crates.
- **MSRV policy** and enforcement.

### Out of scope

- Long-term-support branches beyond the documented window.

## Dependencies

- [112](112-format-upgrade-and-migration.md): format upgrade.
- [201](201-native-protocol-definition.md): the protocol.
- [216](216-packaging-and-deployment.md): packaging.

## Acceptance Criteria

- [ ] The compatibility policy is published.
- [ ] The cross-version matrix runs on every release candidate and passes.
- [ ] `cargo-semver-checks` blocks accidental breaking API changes (verified by a test PR).
- [ ] The release checklist has been run end to end for a release candidate.

## Testing and Validation

- CI matrix jobs.

## References

- [Guide §8.1](../architecture/implementation-guide.md#81-protocol), [§15](../architecture/implementation-guide.md#15-definition-of-done-for-pizzlydb-core)
- `.github/workflows/release-please.yml`
- [cargo-semver-checks](https://github.com/obi1kenobi/cargo-semver-checks)
