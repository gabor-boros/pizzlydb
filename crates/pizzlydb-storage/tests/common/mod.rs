//! Helpers shared by the engine integration suites.
//!
//! Each integration binary compiles this file on its own, so a helper used by
//! one suite is unused in the others.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pizzlydb_storage::{
    Durability, FileRole, Lsn, MemoryVfs, OpenOptions, ReadView, Storage, StorageError, Vfs,
    VfsFile,
};

pub fn fill(byte: u8) -> impl FnOnce(&mut [u8; 16]) {
    move |id| id.fill(byte)
}

pub fn records(db: &Storage<impl Vfs>) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let view = db.snapshot().expect("snapshot");
    scan_view(&view)
}

pub fn scan_view(view: &ReadView<'_, impl Vfs>) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut out = BTreeMap::new();
    for item in view.scan(None).expect("scan") {
        let (key, value) = item.expect("entry");
        out.insert(key, value);
    }
    out
}

pub fn apply(
    db: &Storage<impl Vfs>,
    writes: &[(&[u8], Option<&[u8]>)],
    durability: Durability,
) -> Lsn {
    apply_result(db, writes, durability).expect("commit")
}

pub fn apply_result(
    db: &Storage<impl Vfs>,
    writes: &[(&[u8], Option<&[u8]>)],
    durability: Durability,
) -> Result<Lsn, StorageError> {
    let mut batch = db.begin_write()?;
    for (key, value) in writes {
        match value {
            Some(bytes) => batch.put(key, bytes)?,
            None => batch.delete(key)?,
        }
    }
    batch.commit(durability)
}

pub fn wal_path(path: &Path) -> PathBuf {
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };
    let mut suffixed = name.to_os_string();
    suffixed.push("-wal");
    let mut out = path.to_path_buf();
    out.set_file_name(suffixed);
    out
}

pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).expect("metadata").len()
}

pub fn truncate_file(path: &Path, len: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for truncate");
    file.set_len(len).expect("truncate");
}

pub fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read file")
}

pub fn read_memory(vfs: &MemoryVfs, path: &Path, role: FileRole) -> Vec<u8> {
    let mut file = vfs
        .open(path, role, &OpenOptions::new())
        .expect("open memory file");
    let len = usize::try_from(file.len().expect("len")).expect("len fits");
    let mut buf = vec![0; len];
    if !buf.is_empty() {
        file.read_exact(0, &mut buf).expect("read");
    }
    buf
}

#[cfg(unix)]
pub fn posix_paths(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(name);
    let wal = wal_path(&path);
    (dir, path, wal)
}

pub fn three_batches(db: &Storage<impl Vfs>) -> Vec<BTreeMap<Vec<u8>, Vec<u8>>> {
    let mut history = vec![records(db)];
    apply(
        db,
        &[(b"a", Some(b"1")), (b"b", Some(b"2"))],
        Durability::Full,
    );
    history.push(records(db));
    apply(db, &[(b"a", None), (b"c", Some(b"3"))], Durability::Full);
    history.push(records(db));
    apply(db, &[(b"b", Some(b"9"))], Durability::Full);
    history.push(records(db));
    history
}

pub fn assert_committed_prefix(
    history: &[BTreeMap<Vec<u8>, Vec<u8>>],
    got: &BTreeMap<Vec<u8>, Vec<u8>>,
    len: usize,
) {
    assert!(
        history.iter().any(|map| map == got),
        "prefix {len} matched none of the committed maps: {got:?}"
    );
}
