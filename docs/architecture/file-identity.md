# File identity

Each database file set has a 16-byte file id. Both superblock copies and the WAL header store it. Opening a WAL whose id differs from the superblock returns `StorageError::Corrupt`.

`backup`, `export_checkpoint`, `restore`, `salvage`, and `bulk_load` draw a new id from the system CSPRNG. A backup does not keep the source id, so placing the source WAL next to the copy is rejected.

The superblock body has no lineage field. Bytes after the body must stay zero, and a lineage id would change the format version. [105](../roadmap/105-online-consistent-backup.md) records lineage in the backup manifest.
