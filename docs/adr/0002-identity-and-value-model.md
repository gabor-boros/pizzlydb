# ADR-002: Property-graph identity, value types, key encoding, and schema model

- Status: Reserved
- Owner issues: [034](../roadmap/034-id-allocation-and-identity-policy.md) (part 1, identity) and [040](../roadmap/040-type-system-and-value-model.md) (part 2, types and values)
- Guide: [§3.1 entities](../architecture/implementation-guide.md#31-entities), [§3.3 schema model](../architecture/implementation-guide.md#33-schema-model), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)

This number is reserved. No decision is recorded yet. The owner issues replace this file with the ADR, using the [ADR template](template.md).

## Already fixed elsewhere

Guide §3.1 fixes node and edge identifiers as distinct 128-bit opaque types. The code implements this as 16-byte identifiers (`pizzlydb_core::ID_LEN`).

## Open questions

- Whether clients may supply ids, how long tombstones live, and how "deleted" differs from "never existed" ([034](../roadmap/034-id-allocation-and-identity-policy.md)).
- How ids are allocated without making every create read the same counter key ([034](../roadmap/034-id-allocation-and-identity-policy.md)).
- The numeric total order across integers and floats, and the decimal, temporal, and collection types ([040](../roadmap/040-type-system-and-value-model.md)).
