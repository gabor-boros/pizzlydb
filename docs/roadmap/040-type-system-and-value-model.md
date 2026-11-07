# 040: Type-system RFC and value model completion (ADR-002, part 2)

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must (decision gate; must land before the format freeze) |
| Size | XL |
| Crates | `pizzlydb-core` (`value`, `key`), `pizzlydb-graph` (cells, index keys), later `pizzlydb-query` |
| Type | RFC, feature, on-disk format |

## Summary

Write the type-system RFC that guide §3.1 requires "before storage format freeze", then implement it. The RFC specifies the full property value set, comparison and coercion rules, the sortable key encoding of each indexable type, null and absence semantics, string collation, canonical encoding of names, size limits, and the index policy for values that exceed the maximum key length.

The full value set adds these types to the existing scalars:

- `Decimal`
- `Timestamp` (a UTC instant, with explicitly decided precision)
- `Date`, `LocalDateTime`, and `Duration`, if accepted by the RFC
- `List` (homogeneous or heterogeneous)
- `Map` (nested)

## Motivation

The current `Value` has `Null`, `Bool`, `Int(i64)`, `Float(f64)`, `String`, and `Bytes`. The guide requires `decimal`, `timestamp`, `list`, and nested `map`.

The key encoding orders values by type tag first: null, then bool, then int, then float, then string, then bytes. As a result `Int(2)` sorts before `Float(1.5)`, so a range query `x > 1.5` over an index of mixed numeric values gives wrong results unless the query layer compensates. Whether numbers compare across types has to be decided once, written down, and encoded in the index key format. Changing it after the format freeze would require a rewrite of every index.

String comparison is plain byte order, and catalog names are stored as exact bytes. The guide requires "canonical encoding and defined collation rules" for names.

## Current State

- `crates/pizzlydb-core/src/value.rs`: six variants. NaN is rejected and `-0.0` is normalized to `+0.0`. Strings in keys use `0x00` escaping with a `0x00 0x01` terminator, and the tag order is null < bool < int < float < string < bytes (around lines 37–39).
- `crates/pizzlydb-core/src/key.rs`: `KeyBuf`, a namespace followed by a kind byte.
- `crates/pizzlydb-storage/src/btree.rs`: `MAX_KEY_LEN = 512`. The graph crate rejects index keys that are too long (`ValueTooLarge`), but the user-visible policy is undocumented.

## Scope

### In scope

- **RFC (`docs/rfcs/`).** It specifies:
  - the value set and the precision of each type: decimal width (for example 128-bit with a scale limit) and timestamp resolution (microseconds or nanoseconds)
  - equality, ordering, and coercion between `Int`, `Float`, and `Decimal`. The recommendation is a single numeric order in index keys, using an order-preserving encoding of exact numeric value, so that indexed numeric ranges stay correct across numeric types.
  - comparability of lists and maps (whether ordered comparison is allowed and whether they can be indexed)
  - three-valued logic inputs for the query layer
  - string collation: binary UTF-8 for v1, with any locale collation deferred
  - name canonicalization: NFC normalization or exact bytes, and case sensitivity
  - maximum encoded sizes, and the behavior for index keys longer than `MAX_KEY_LEN` (reject at write time with a typed error, or index a prefix plus a hash and recheck)
- **Implementation.** New variants in `pizzlydb-core::Value` and `ValueType`. Cell and key encoders and decoders, with property tests that check byte order matches the comparator.
- **Graph changes.** Declarations accept the new types. The cross-numeric index encoding applies to the property and unique index keys.
- **Fuzz targets.** One per new decoder ([006](006-fuzzing-infrastructure.md)).

### Out of scope

- Query expression semantics beyond the comparison rules ([050](050-query-language-specification.md)).
- Spatial, vector, and other specialized types ([904](904-composite-and-specialized-indexes.md)).

## Dependencies

- [001](001-design-record-and-baseline-adrs.md): the RFC home.

## Acceptance Criteria

- [ ] The RFC is accepted, with a comparison table covering every pair of types, coercion rules, and worked examples, including `Int(2)` against `Float(1.5)` and `Decimal("1.50")` against `Int(1)`.
- [ ] Property test: for random value pairs, `cmp(a, b) == memcmp(encode_key(a), encode_key(b))` for every indexable type.
- [ ] Round-trip property tests and fuzzing for every new encoding.
- [ ] A mixed-numeric index range query returns exactly the values that the RFC comparator selects (graph integration test).
- [ ] The behavior for over-length index values is implemented, documented, and tested.

## Testing and Validation

- Property tests, fuzz targets, and graph integration tests for declarations and indexes using the new types.

## References

- [Guide §3.1](../architecture/implementation-guide.md#31-entities), [§3.4](../architecture/implementation-guide.md#34-initial-physical-index-set)
- `crates/pizzlydb-core/src/{value,key}.rs`, `crates/pizzlydb-graph/src/{property,index}.rs`
- ISO/IEC 39075 (GQL) value types
