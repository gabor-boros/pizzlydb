# RFC 0001: Property value types, comparison, and index keys

- Status: Accepted
- Owner issue: [040](../roadmap/040-type-system-and-value-model.md)
- Guide: [§3.1](../architecture/implementation-guide.md#31-entities), [§3.4](../architecture/implementation-guide.md#34-initial-physical-index-set)
- Supersedes: none

## Problem and workload

Property cells stored null, bool, `i64`, `f64`, UTF-8 strings, and bytes. Index keys ordered those types by tag, so `Int(2)` sorted before `Float(1.5)`. A range over mixed numbers was wrong. The guide also requires decimal, timestamp, lists, and nested maps, plus a defined collation for names, before the storage format is treated as frozen.

## User-facing contract

`Value` is null, bool, int (`i64`), float (`f64`), decimal, string, bytes, timestamp, date, local date-time, duration, list, or map.

Absence is a missing property key. Null is a stored `Value::Null`. They are not equal.

| Type | Precision |
| --- | --- |
| int | `i64` |
| float | IEEE-754 binary64. NaN is rejected. `-0.0` is stored as `+0.0` |
| decimal | signed coefficient in `i128`, at most 38 digits, scale `0..=28`. Trailing fractional zeros are removed, so `1.50` and `1.5` are the same value |
| timestamp | UTC microseconds since the Unix epoch, full `i64` |
| date | proleptic Gregorian days since 1970-01-01, `i32` |
| local date-time | civil microseconds since 1970-01-01T00:00:00, no offset, `i64` |
| duration | signed microseconds, `i64` |
| list | heterogeneous, nesting at most 32 |
| map | string keys, unique, byte-sorted, nesting at most 32 |

No storage coercion. A declaration of int does not accept a float or a decimal.

### Comparison groups

`predicate_cmp` returns unknown (`None`) when either side is null, a float is NaN, or the values are in different groups. `index_cmp` is a total order used by index keys. Null sorts first. NaN cannot be stored.

| Group | Order |
| --- | --- |
| null | only null |
| bool | `false` < `true` |
| number | int, float, and decimal, by exact mathematical value |
| timestamp | by instant |
| date | by day |
| local date-time | by civil microseconds. Not equal to a timestamp with the same count |
| duration | by microseconds |
| string | unsigned UTF-8 bytes |
| bytes | unsigned bytes |
| list | lexicographic `index_cmp` of elements, then a shorter list is smaller |
| map | lexicographic by key bytes, then by value `index_cmp` |

Worked numeric examples:

| Left | Right | `index_cmp` |
| --- | --- | --- |
| `Int(2)` | `Float(1.5)` | greater |
| `Decimal("1.50")` | `Int(1)` | greater |
| `Int(1)` | `Float(1.0)` | equal |
| `Int(1)` | `Decimal("1.00")` | equal |
| `Float(0.1)` | `Decimal("0.1")` | greater, because the float is not the decimal tenth |

Equal numbers encode to the same index key. Decoding that key returns one canonical variant of the same exact value. The cell encoding still round-trips the original variant.

Lists and maps may be indexed. An index key longer than `MAX_KEY_LEN` (512 bytes, including the namespace, kind, index id, and owner) is rejected at write time with `GraphError::KeyTooLarge`. The index does not store a prefix or a hash.

Names are exact UTF-8 bytes, case-sensitive, with no Unicode normalization. Empty names, NUL bytes, and names longer than 256 bytes are rejected. Catalog ids are never retired and never reused. Rename is the only way to change a name.

## Data and control flow

Cells use a type tag so the stored variant is recovered exactly. Index keys use a separate group tag. The number group is one order-preserving decimal encoding of the exact value, including the exact decimal expansion of a binary float. Timestamp, date, local date-time, and duration use the existing order-preserving integer encodings.

## Correctness invariants

For every pair of encodable values, `index_cmp(a, b)` equals the unsigned byte order of their index keys. A numeric range over an index returns every stored number the comparator selects, regardless of which numeric variant was written.

## On-disk and protocol changes

The graph logical format version stays 1. Cell tags `7` through `13` are decimal, timestamp, date, local date-time, duration, list, and map. Index key group tags are not the same bytes as the cell tags for strings and numbers. Property cells use one key per owner and property. Declaration bytes gained a required flag in the existing flag byte. Index definitions gained a phase, a backfill flag, and a progress id. None of these changes bump `FORMAT_MAJOR`, `FORMAT_MINOR`, the page version, the WAL version, or `GRAPH_LOGICAL_VERSION`.

## Security and threats

Values are bounded by the storage value limit and by `u32` length prefixes. Decoders check the length before they allocate. Nesting deeper than 32 is rejected. Names are not normalized, so lookalike characters remain distinct.

## Resource limits

Decimal scale is at most 28 and the coefficient has at most 38 digits. Nested values stop at depth 32. Index keys stop at 512 bytes. Over-long keys fail the write.

## Failure and recovery

A NaN, an out-of-range decimal, or an over-deep value fails the encoding and stages nothing. An over-long index key fails the graph write and stages nothing. Crash recovery replays committed cells; it does not rewrite index key encodings.

## Alternatives

- Keep a separate float tag in the index key. Rejected. Mixed numeric ranges stay wrong.
- Canonicalize every number to decimal in the cell. Rejected. `Float(0.1)` is not `Decimal("0.1")`, and callers need the original variant back.
- Locale collation and NFC names. Deferred. v1 compares exact bytes.
- Index a prefix plus a hash when a key is too long. Rejected for v1. The write fails instead of returning a false match.

## Compatibility and migration

There is no released database to migrate. Encodings changed in place. The logical format version was not increased.

## Test oracle

`index_cmp` against the encoded bytes, cell round-trips, and a graph index range that includes `Int(2)` and excludes `Float(1.5)` and `Decimal("1.50")` when the lower bound is exclusive `Float(1.5)`.

## Benchmark and release gate

No new benchmark. The index range test is part of `cargo test -p pizzlydb-graph --tests`.

## Open questions

None. Locale collation, NFC names, and prefix indexing of over-long values remain future work and are out of scope for v1.
