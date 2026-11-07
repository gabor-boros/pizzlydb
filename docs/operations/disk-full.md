# Disk full

`ENOSPC` and quota errors (`EDQUOT`, raw 69 or 122) are `VfsError::NoSpace`. The storage handle maps that to `StorageError::NoSpace` and sets a degraded flag. Later `begin_write` calls return `NoSpace`. `snapshot` still reads the last committed sequence. `sync` failure is not degraded mode: it closes the handle.

Free space on the volume, then reopen the database. The degraded handle does not start accepting writes again.

`preallocate` extends the file with `set_len` when the requested range is past the current length. That is a hole on filesystems that do not reserve blocks. It is not `posix_fallocate`.
