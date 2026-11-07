# 215: CSV and JSON Lines import and export

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Should |
| Size | M |
| Crates | `pizzlydb-cli`, `pizzlydb-graph` (`bulk`, `export`) |
| Type | Feature, integration |

## Summary

Support importing and exporting common interchange formats:

- CSV with a mapping file, in the style of neo4j-admin (node and relationship files, with id-space mapping).
- JSON Lines, one entity per line.

Both work offline, through the bulk loader ([110](110-bounded-memory-bulk-loader.md)), and online, through batched transactions. Validation, error reporting, and resumability are defined explicitly.

## Motivation

Adoption depends on getting data in and out. The native logical export ([111](111-logical-export-import.md)) is lossless but specific to pizzlydb. Common formats are the integration path with ETL tools.

## Current State

- Bulk load accepts only programmatic input. There are no file-format readers.

## Scope

### In scope

- **Mapping specification:** column-to-property mapping, type coercion rules (strict by default), label and relationship-type columns, and handling of external ids.
- **Readers and writers.** Streaming, with bounded memory.
- **Error policy.** Fail fast, or skip and report with a cap. The report includes the line number and the reason.
- **Resume support.** Offline loads resume from a checkpointed offset.
- **CLI commands** `import` and `export` ([213](213-cli.md)).

### Out of scope

- Parquet and Arrow, a possible later RFC.

## Dependencies

- [110](110-bounded-memory-bulk-loader.md): the bulk loader.
- [111](111-logical-export-import.md): logical export and import.
- [213](213-cli.md): the CLI.

## Acceptance Criteria

- [ ] A round trip from CSV to the database and back to CSV preserves the mapped data (property test).
- [ ] Malformed input produces precise errors, and the policy is honored (tests).
- [ ] An import of 100 million edges completes within the bulk-loader memory budget, measured.

## Testing and Validation

- Round-trip tests, malformed-input tests, fuzzing of the parsers, and a large-scale run.

## References

- [Guide §10.2](../architecture/implementation-guide.md#102-backups-and-restore)
