# Storage compatibility fixtures

`superblock-v1.0.bin` is one sealed superblock page: major 1, minor 0, log generation 1, feature flags 0. `superblock::tests::golden_superblock_is_format_1_0` decodes it. Format 1.0 is the only superblock version. A feature bit outside `KNOWN_FEATURE_FLAGS` is `UnsupportedVersion`.

Regenerate only when the encoder changes and the fixture is supposed to follow:

```sh
PIZZLYDB_WRITE_FIXTURE=1 cargo test -p pizzlydb-storage --lib -- golden_superblock_is_format_1_0 -- --exact
```
