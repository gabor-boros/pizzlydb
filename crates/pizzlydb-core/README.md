# pizzlydb-core

Shared identifiers, scalar values, and ordered key encodings.

## Purpose

`pizzlydb-core` is the shared type layer for the database crates. It defines opaque identifiers, the property value set, and the byte encodings that higher crates use as keys and values in the storage map.

Property absence stays distinct from an explicit null: absence is `Option::None`, null is `Value::Null`. Identifier values do not encode a storage location.

The crate has no dependency on storage, transactions, the graph layout, query planning, or the network protocol.

## Public types

- `NamespaceId`, `NodeId`, `EdgeId`, `LabelId`, `TypeId`, `PropertyKeyId`, and `IndexId` are 16-byte identifiers. Zero is invalid. The big-endian bytes sort in numeric order.
- `Value` is `Null`, `Bool`, `Int` (`i64`), `Float` (`f64`), `String`, or `Bytes`. `ValueType` names the non-null types.
- `encode_value` and `decode_value` write and read the stored form of a value: a type tag, then the body. Strings and byte strings carry a `u32` length.
- `encode_value_key` and `decode_value_key` write and read the key form: a type tag, then a body whose unsigned byte order equals value order. No encoded value is a prefix of another, so tuples of values and identifiers sort field by field.
- `encode_i64_key` and `encode_f64_key` write the 8-byte ordered form of one number.
- `KeyBuf` builds a key that starts with a namespace and a kind byte.
- `CoreError` is returned for a zero identifier, an invalid value, or truncated input.

Every encoder rejects NaN and writes `-0.0` as `+0.0`. Every decoder rejects malformed tags, trailing bytes, and a length prefix longer than the input before it allocates.

## Modules

- `id` — identifiers
- `value` — values and both encodings
- `key` — key builder
- `sequence` — monotonic `CommitSeq` values (not yet implemented)
- `error` — errors for those types

## License

Licensed under the [Apache License 2.0](../../LICENSE).
