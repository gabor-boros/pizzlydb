# 111: Logical export and import (portable dump)

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-graph`, `pizzlydb-engine`, `pizzlydb-cli` |
| Type | Feature |

## Summary

Stream a consistent snapshot of one namespace or of all namespaces into a documented, versioned, format-independent dump, and import a dump through the bulk loader ([110](110-bounded-memory-bulk-loader.md)). The dump contains:

- the namespace metadata and the catalog names
- the declarations, constraints, and index definitions
- the nodes with their labels and properties
- the edges with their properties
- the optional definitions of maintained plans

The dump is the portable path across incompatible format versions, across architectures, and into external tools.

## Motivation

A physical backup only works for the same major format. Production databases need a logical path for three cases:

- major upgrades that cannot run in place ([112](112-format-upgrade-and-migration.md))
- moving or splitting namespaces
- auditing and debugging

Without one, an incompatible format change strands existing data.

## Current State

- No export exists, and the only loader input is the engine-native bulk script ([110](110-bounded-memory-bulk-loader.md)).

## Scope

### In scope

- **Dump format v1.** A header with format and source versions, the lineage, the S frontier, and the namespaces. Records follow in a documented order, as JSON Lines or a CBOR sequence; the ADR chooses, weighing readability against size and type fidelity.
- **Type fidelity.** The encoding is exact for every type in [040](040-type-system-and-value-model.md): decimals as strings, explicit timestamp precision, and bytes as base64 in JSON.
- **Export.** `export(snapshot, namespaces, writer)` streams in bounded memory, with optional compression and per-chunk checksums.
- **Import.** `import(reader, destination, mapping)` reuses the bulk loader. Ids are either preserved, which requires an allocator high-water fix-up, or remapped, which is documented.
- **Round-trip property test:** export, import, export again, and compare. The results must be identical apart from header metadata.

### Out of scope

- CSV and JSONL ingestion for third-party data ([215](215-csv-jsonl-import-export.md)).

## Dependencies

- [110](110-bounded-memory-bulk-loader.md): the import path.
- [040](040-type-system-and-value-model.md): value fidelity.
- [043](043-schema-and-catalog-lifecycle.md): schema elements.

## Acceptance Criteria

- [ ] The round trip is identical for random graphs that cover every value type and schema element (property test).
- [ ] Exporting 100 million edges runs in bounded memory (benchmark).
- [ ] The format is documented in `docs/specifications/dump-format.md`, and a decoder fuzz target exists.

## Testing and Validation

- Property tests, fuzzing, and a benchmark.

## References

- [Guide §10.2](../architecture/implementation-guide.md#102-backups-and-restore)
