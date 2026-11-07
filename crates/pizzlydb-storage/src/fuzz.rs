//! Decoders exercised by the `fuzz/` workspace.
//!
//! Enable this module with `--features fuzzing`. Each function returns a
//! typed error for input it rejects and panics only when a constructed value
//! fails its own round trip.

use std::hint::black_box;
use std::path::Path;

use crate::btree::{decode_leaf_entry, decode_overflow_page, fields_ok};
use crate::engine::{Durability, Storage};
use crate::page::{HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind};
use crate::space::{self, SpillImage};
use crate::superblock::{self, Superblock};
use crate::verify::verify_quick;
use crate::vfs::{FileRole, OpenOptions, Vfs, VfsFile};
use crate::vfs_memory::MemoryVfs;
use crate::wal::{self, WalFrame};
use crate::wal_path;

/// Page header decode, plus `decode(encode(header))` for one header taken from `bytes`.
pub fn page_header(bytes: &[u8]) {
    let _ = black_box(PageHeader::decode(bytes));
    let Some(kind) = bytes
        .first()
        .copied()
        .and_then(|tag| PageKind::try_from(u16::from(tag)).ok())
    else {
        return;
    };

    let header = PageHeader::new(
        kind,
        PageId::new(u64_at(bytes, 1)),
        Lsn::new(u64_at(bytes, 9)),
    );

    let mut page = vec![0u8; PAGE_SIZE];
    let body = bytes.len().min(PAGE_SIZE - HEADER_SIZE);
    page[HEADER_SIZE..HEADER_SIZE + body].copy_from_slice(&bytes[..body]);
    header.encode(&mut page).expect("page image is one page");
    assert_eq!(PageHeader::decode(&page).ok(), Some(header));
}

/// Superblock decode, plus a round trip of one valid record.
pub fn superblock(bytes: &[u8]) {
    let _ = black_box(Superblock::decode(bytes, superblock::COPY_A));
    let _ = black_box(Superblock::decode(bytes, superblock::COPY_B));
    let mut file_id = [0u8; 16];
    let prefix = bytes.len().min(16);
    file_id[..prefix].copy_from_slice(&bytes[..prefix]);

    let Ok(record) = Superblock::new(
        file_id,
        1,
        Lsn::new(1),
        8,
        PageId::new(2),
        PageId::new(0),
        PageId::new(0),
    ) else {
        return;
    };

    let page = record.encode(superblock::COPY_A).expect("copy A");
    assert_eq!(
        Superblock::decode(&page, superblock::COPY_A).ok(),
        Some(record)
    );
}

/// WAL header decode, plus `decode(encode(file_id))`.
pub fn wal_header(bytes: &[u8]) {
    let _ = black_box(wal::decode_header(bytes));
    let mut file_id = [0u8; 16];
    let prefix = bytes.len().min(16);
    file_id[..prefix].copy_from_slice(&bytes[..prefix]);
    let header = wal::encode_header(&file_id).expect("wal header");
    assert_eq!(wal::decode_header(&header).ok(), Some(file_id));
}

/// WAL frame decode, plus a round trip of one commit frame.
pub fn wal_frame(bytes: &[u8]) {
    let _ = black_box(wal::read_frame(bytes));
    let mut payload = [0u8; 24];
    payload[0..8].copy_from_slice(&1u64.to_le_bytes());
    payload[8..16].copy_from_slice(&2u64.to_le_bytes());
    payload[16..24].copy_from_slice(&1u64.to_le_bytes());
    let frame = wal::encode_frame(wal::KIND_COMMIT, &payload).expect("commit frame");
    let (decoded, len) = wal::read_frame(&frame).expect("commit frame decodes");
    assert_eq!(len, frame.len());
    assert_eq!(
        decoded,
        WalFrame::Commit {
            txn_id: 1,
            lsn: Lsn::new(2),
            page_count: 1,
        }
    );
}

/// B-tree node header and one leaf entry.
pub fn btree_node(bytes: &[u8]) {
    let _ = black_box(fields_ok(bytes));
    let _ = black_box(decode_leaf_entry(bytes).is_ok());
}

/// One overflow page image.
pub fn overflow(bytes: &[u8]) {
    let _ = black_box(decode_overflow_page(bytes));
}

/// Allocator header.
pub fn allocator(bytes: &[u8]) {
    let _ = black_box(space::header_ok(bytes));
}

/// Allocation bitmap page.
pub fn bitmap(bytes: &[u8]) {
    let _ = black_box(space::decode_bitmap(bytes));
}

/// Retire-spill page and a one-page chain.
pub fn retire_spill(bytes: &[u8]) {
    if bytes.len() < PAGE_SIZE {
        let _ = black_box(space::spill_chain_ok(0, &[], 8));
        return;
    }
    let mut image = [0u8; PAGE_SIZE];
    image.copy_from_slice(&bytes[..PAGE_SIZE]);
    let spill = SpillImage {
        page_id: PageId::new(4),
        bytes: image,
    };
    let _ = black_box(space::spill_image_ok(&spill, 8));
    let _ = black_box(space::spill_chain_ok(4, &[spill], 8));
}

/// Open a database whose files are `bytes`, then read it.
///
/// [`Storage::open`](crate::Storage::open) returns [`Ok`](crate::Storage) or a
/// [`StorageError`](crate::StorageError). A panic is a decoder bug.
pub fn engine(bytes: &[u8]) {
    let vfs = MemoryVfs::new();
    let path = Path::new("db");
    let options = OpenOptions::new().create(true);

    if let Ok(mut file) = vfs.open(path, FileRole::Main, &options) {
        let _ = file.write_all(0, bytes);
        let _ = file.sync();
    }

    if let Ok(mut file) = vfs.open(&wal_path(path), FileRole::Wal, &options) {
        let split = bytes.len() / 2;
        let _ = file.write_all(0, bytes.get(split..).unwrap_or(bytes));
        let _ = file.sync();
    }

    let _ = vfs.sync_dir(Path::new("."));
    let Ok(db) = Storage::open(&vfs, path, 4, Durability::None) else {
        return;
    };

    let _ = black_box(verify_quick(&vfs, path));
    let Ok(view) = db.snapshot() else {
        return;
    };

    let key = bytes.get(..bytes.len().min(32)).unwrap_or(b"");
    let _ = black_box(view.get(key));
    let Ok(scan) = view.scan(None) else {
        return;
    };

    for item in scan.take(8) {
        let _ = black_box(item);
    }
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    if let Some(slice) = bytes.get(offset..offset + 8) {
        raw.copy_from_slice(slice);
    }
    u64::from_le_bytes(raw)
}
