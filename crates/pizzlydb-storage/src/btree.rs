//! Bytewise B+ tree of opaque keys and values.
//!
//! Order is unsigned byte order. A shorter key that is a prefix of a longer
//! key is smaller. [`BTree::insert`] takes the key and the value bytes and
//! does not take a comparator. Callers that need numeric order encode it into
//! the key before insert.
//!
//! The caller owns the root [`PageId`]. Zero means the tree has no pages.
//! This module does not read or write the superblock.
//!
//! # Node layout
//!
//! Leaves are [`PageKind::BTreeLeaf`]. Internal pages are
//! [`PageKind::BTreeInternal`]. Both use the standard page header. The next
//! 40 bytes of the body are:
//!
//! - `0..8`: right sibling, or zero on an internal page
//! - `8..16`: left sibling, or zero on an internal page
//! - `16..24`: parent, or zero for the root
//! - `24..32`: leftmost child, or zero on a leaf
//! - `32..34`: entry count (`u16`)
//! - `34..36`: payload length (`u16`)
//! - `36..40`: reserved zero (`u32`)
//!
//! Packed entries follow, at body offset 40. Their length is the payload.
//! After a mutation returns, a non-root node holds at least [`MIN_PAYLOAD`]
//! bytes when a sibling can supply or receive an entry and still leave both
//! nodes at that minimum, or when the two nodes fit in one page. Otherwise
//! the node keeps the entries it has. The root may be smaller. Removing the
//! last key retires the root. An internal root whose only child is the
//! leftmost child is retired, and that child becomes the root.
//!
//! A separator is the smallest key of the child on its right. That key stays
//! in the child. Every key in the left subtree compares smaller. Every key in
//! the right subtree compares greater or equal. Search follows that rule.
//!
//! # Entries
//!
//! A leaf entry is a `u16` key length, a `u8` flags byte, a reserved `u8`,
//! the key, then either an inline value or an overflow reference.
//!
//! - Flags `0`: `u32` value length and the value bytes.
//! - Flags `1`: `u64` overflow head and `u64` value length.
//!
//! An internal entry is a `u16` key length, a reserved `u16`, the key, and a
//! `u64` child page id.
//!
//! Keys longer than [`MAX_KEY_LEN`] are rejected with [`TreeError::KeyTooLarge`].
//! The empty key is stored and sorts first.
//!
//! # Overflow
//!
//! A value whose inline leaf entry is larger than [`BODY_CAP`] is stored on
//! [`PageKind::Overflow`] pages. The leaf keeps the key. An overflow page
//! holds a `u64` next page, a `u32` chunk length, a reserved `u32`, and then
//! [`OVERFLOW_CHUNK`] data bytes. A chain longer than [`MAX_OVERFLOW_HOPS`],
//! a length above [`MAX_VALUE_LEN`], a zero chunk, or a length that does not
//! match the chain is [`TreeError::Corrupt`]. The stored length is checked
//! before the value buffer is allocated.
//!
//! # Cursor
//!
//! [`Cursor`] does not implement [`Copy`] or [`Clone`]. [`Cursor::next`]
//! borrows the cursor mutably. The key and value from one call cannot be used
//! after the next call. An empty scan, and the call that ends a scan, do not
//! allocate a page or a buffer.

use std::cmp::Ordering;
use std::ops::{Deref, DerefMut};

use thiserror::Error;

use crate::codec::{get_u16, get_u32, get_u64, put_u16, put_u32, put_u64};
use crate::page::{HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind};

/// Maximum key length in bytes.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::MAX_KEY_LEN;
///
/// assert_eq!(MAX_KEY_LEN, 256);
/// ```
pub(crate) const MAX_KEY_LEN: usize = 256;

/// Maximum number of parent links followed from a leaf to the root.
pub(crate) const MAX_TREE_HOPS: u32 = 64;

/// Maximum pages in one overflow chain.
pub(crate) const MAX_OVERFLOW_HOPS: u32 = 64;

/// Upper bound on a sibling walk.
///
/// Tree height uses [`MAX_TREE_HOPS`]. A leaf chain is wider than the tree is
/// tall, so the cycle limit for that walk is separate.
const MAX_LEAF_HOPS: u32 = 1_048_576;

const RIGHT_OFF: usize = HEADER_SIZE;
const LEFT_OFF: usize = HEADER_SIZE + 8;
const PARENT_OFF: usize = HEADER_SIZE + 16;
const LEFT_CHILD_OFF: usize = HEADER_SIZE + 24;
const COUNT_OFF: usize = HEADER_SIZE + 32;
const PAYLOAD_OFF: usize = HEADER_SIZE + 34;
const RESERVED_OFF: usize = HEADER_SIZE + 36;
const BODY_OFF: usize = HEADER_SIZE + 40;

/// Bytes available for packed entries on a leaf or internal page.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::{BODY_CAP, MIN_PAYLOAD};
///
/// assert!(MIN_PAYLOAD * 2 <= BODY_CAP);
/// assert!(BODY_CAP > 256);
/// ```
pub(crate) const BODY_CAP: usize = PAGE_SIZE - BODY_OFF;

/// Minimum payload of a non-root node after insert or delete returns.
pub(crate) const MIN_PAYLOAD: usize = BODY_CAP / 2;

const LEAF_INLINE_OVERHEAD: usize = 8;
const LEAF_OVERFLOW_OVERHEAD: usize = 20;
const INTERNAL_OVERHEAD: usize = 12;

const FLAG_INLINE: u8 = 0;
const FLAG_OVERFLOW: u8 = 1;

const OV_NEXT_OFF: usize = HEADER_SIZE;
const OV_LEN_OFF: usize = HEADER_SIZE + 8;
const OV_RESERVED_OFF: usize = HEADER_SIZE + 12;
const OV_DATA_OFF: usize = HEADER_SIZE + 16;

/// Data bytes stored on one overflow page.
pub(crate) const OVERFLOW_CHUNK: usize = PAGE_SIZE - HEADER_SIZE - 16;

/// Maximum value length accepted by [`BTree::insert`].
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::{MAX_OVERFLOW_HOPS, MAX_VALUE_LEN, OVERFLOW_CHUNK};
///
/// assert_eq!(MAX_VALUE_LEN, MAX_OVERFLOW_HOPS as usize * OVERFLOW_CHUNK);
/// ```
pub(crate) const MAX_VALUE_LEN: usize = MAX_OVERFLOW_HOPS as usize * OVERFLOW_CHUNK;

const _: () = assert!(BODY_OFF + BODY_CAP == PAGE_SIZE);
const _: () = assert!(OV_DATA_OFF + OVERFLOW_CHUNK == PAGE_SIZE);
const _: () = assert!(MIN_PAYLOAD * 2 <= BODY_CAP);
const _: () = assert!(MAX_KEY_LEN < BODY_CAP);
const _: () = assert!(LEAF_OVERFLOW_OVERHEAD + MAX_KEY_LEN <= MIN_PAYLOAD);

/// Read or write failure for a B+ tree.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::TreeError;
///
/// assert_eq!(TreeError::KeyTooLarge, TreeError::KeyTooLarge);
/// assert_ne!(TreeError::Corrupt, TreeError::ValueTooLarge);
/// ```
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum TreeError {
    /// A page header, link, separator, key order, or overflow chain is invalid.
    #[error("corrupt tree page")]
    Corrupt,
    /// The key is longer than [`MAX_KEY_LEN`].
    #[error("key is too long")]
    KeyTooLarge,
    /// The value is longer than [`MAX_VALUE_LEN`].
    #[error("value is too long")]
    ValueTooLarge,
    /// The page allocator has no free page id.
    #[error("no free page")]
    Full,
    /// The page id is already free.
    #[error("page is already free")]
    DoubleFree,
}

/// Shared access to sealed page images.
///
/// `pin` returns the stored bytes. The tree checks the checksum and the page
/// id before it reads the body, and it checks the kind before it follows a
/// child or a sibling.
pub(crate) trait PageRead {
    /// Borrowed page image.
    type Pin<'a>: Deref<Target = [u8]>
    where
        Self: 'a;

    /// Copy source for page `id`.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Corrupt`] when `id` is not a stored page.
    fn pin(&self, id: PageId) -> Result<Self::Pin<'_>, TreeError>;
}

/// Allocate, retire, and replace page images.
///
/// The tree drops each pin before it pins or allocates another page.
pub(crate) trait PageWrite: PageRead {
    /// Mutable page image.
    type PinMut<'a>: DerefMut<Target = [u8]>
    where
        Self: 'a;

    /// Allocate a page of `kind` and return its id.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Full`] when no id remains, and
    /// [`TreeError::Corrupt`] when `kind` cannot be allocated.
    fn allocate(&mut self, kind: PageKind) -> Result<PageId, TreeError>;

    /// Queue `id` so a later reclaim may reuse it.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::DoubleFree`] when the id is already free, and
    /// [`TreeError::Corrupt`] when the id cannot be retired.
    fn retire(&mut self, id: PageId) -> Result<(), TreeError>;

    /// Mutable image for a page the tree is replacing.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Corrupt`] when `id` is not a stored page.
    fn pin_mut(&mut self, id: PageId) -> Result<Self::PinMut<'_>, TreeError>;
}

/// Ordered map stored on leaf and internal pages.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::BTree;
/// use pizzlydb_storage::page::PageId;
///
/// let tree = BTree::new();
/// assert_eq!(tree.root(), PageId::new(0));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BTree {
    root: PageId,
}

impl BTree {
    /// Empty tree.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::BTree;
    /// use pizzlydb_storage::page::PageId;
    ///
    /// assert_eq!(BTree::new().root(), PageId::new(0));
    /// ```
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            root: PageId::new(0),
        }
    }

    /// Tree whose root page is `root`.
    ///
    /// Zero is an empty tree. This does not read the page.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::BTree;
    /// use pizzlydb_storage::page::PageId;
    ///
    /// let tree = BTree::from_root(PageId::new(4));
    /// assert_eq!(tree.root(), PageId::new(4));
    /// ```
    #[must_use]
    pub(crate) const fn from_root(root: PageId) -> Self {
        Self { root }
    }

    /// Root page, or zero when the tree is empty.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::BTree;
    /// use pizzlydb_storage::page::PageId;
    ///
    /// assert_eq!(BTree::new().root(), PageId::new(0));
    /// ```
    #[must_use]
    pub(crate) const fn root(self) -> PageId {
        self.root
    }

    /// Value stored at `key`, or [`None`] when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::KeyTooLarge`] when `key` is longer than
    /// [`MAX_KEY_LEN`]. Returns [`TreeError::Corrupt`] when a page on the
    /// search path is invalid.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::{BTree, PageWrite};
    ///
    /// fn round_trip<P: PageWrite>(pages: &mut P) -> Result<(), pizzlydb_storage::btree::TreeError> {
    ///     let mut tree = BTree::new();
    ///     tree.insert(pages, b"a", b"1")?;
    ///     assert_eq!(tree.get(pages, b"a")?.as_deref(), Some(&b"1"[..]));
    ///     Ok(())
    /// }
    /// ```
    pub(crate) fn get<P: PageRead>(
        &self,
        pages: &P,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, TreeError> {
        check_key(key)?;

        if is_zero(self.root) {
            return Ok(None);
        }

        let leaf = find_leaf(pages, self.root, key)?;
        let image = load_page(pages, leaf)?;
        let fields = expect_leaf(&image, leaf)?;
        let items = read_leaf_items(&image, &fields)?;
        for item in &items {
            match cmp_bytes(&item.key, key) {
                Ordering::Less => {}
                Ordering::Equal => return Ok(Some(materialize(pages, &item.value)?)),
                Ordering::Greater => return Ok(None),
            }
        }

        Ok(None)
    }

    /// Insert `key` or replace its value.
    ///
    /// An equal value leaves the pages unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::KeyTooLarge`] or [`TreeError::ValueTooLarge`]
    /// before a page is allocated. Returns [`TreeError::Full`] when a split
    /// or an overflow chain needs a page id and none remain. Returns
    /// [`TreeError::Corrupt`] when an existing page is invalid.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::{BTree, PageWrite};
    ///
    /// fn replace<P: PageWrite>(pages: &mut P) -> Result<(), pizzlydb_storage::btree::TreeError> {
    ///     let mut tree = BTree::new();
    ///     tree.insert(pages, b"a", b"1")?;
    ///     tree.insert(pages, b"a", b"2")?;
    ///     assert_eq!(tree.get(pages, b"a")?.as_deref(), Some(&b"2"[..]));
    ///     Ok(())
    /// }
    /// ```
    pub(crate) fn insert<P: PageWrite>(
        &mut self,
        pages: &mut P,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), TreeError> {
        check_key(key)?;
        check_value(value)?;

        if is_zero(self.root) {
            let stored = make_stored(pages, key, value)?;
            let id = pages.allocate(PageKind::BTreeLeaf)?;

            write_leaf(
                pages,
                id,
                PageId::new(0),
                PageId::new(0),
                PageId::new(0),
                &[LeafItem {
                    key: key.to_vec(),
                    value: stored,
                }],
            )?;

            self.root = id;
            return Ok(());
        }

        let leaf = find_leaf(pages, self.root, key)?;
        let image = load_page(pages, leaf)?;
        let fields = expect_leaf(&image, leaf)?;
        let mut items = read_leaf_items(&image, &fields)?;
        if let Ok(index) = items.binary_search_by(|item| cmp_bytes(&item.key, key))
            && values_equal(pages, &items[index].value, value)?
        {
            return Ok(());
        }

        let stored = make_stored(pages, key, value)?;
        let old = upsert(&mut items, key, stored);
        self.store_leaf(pages, leaf, &items)?;
        if let Some(StoredValue::Overflow { head, len }) = old {
            retire_overflow(pages, head, len)?;
        }

        Ok(())
    }

    /// Remove `key`.
    ///
    /// A missing key leaves the pages unchanged and returns [`Ok`].
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::KeyTooLarge`] when `key` is longer than
    /// [`MAX_KEY_LEN`]. Returns [`TreeError::Corrupt`] when a page on the
    /// search path is invalid.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::{BTree, PageWrite};
    ///
    /// fn remove_one<P: PageWrite>(pages: &mut P) -> Result<(), pizzlydb_storage::btree::TreeError> {
    ///     let mut tree = BTree::new();
    ///     tree.insert(pages, b"a", b"1")?;
    ///     tree.remove(pages, b"missing")?;
    ///     tree.remove(pages, b"a")?;
    ///     assert_eq!(tree.get(pages, b"a")?, None);
    ///     Ok(())
    /// }
    /// ```
    pub(crate) fn remove<P: PageWrite>(
        &mut self,
        pages: &mut P,
        key: &[u8],
    ) -> Result<(), TreeError> {
        check_key(key)?;
        if is_zero(self.root) {
            return Ok(());
        }

        let leaf = find_leaf(pages, self.root, key)?;
        let image = load_page(pages, leaf)?;
        let fields = expect_leaf(&image, leaf)?;

        let mut items = read_leaf_items(&image, &fields)?;
        let Some(index) = items
            .iter()
            .position(|item| cmp_bytes(&item.key, key) == Ordering::Equal)
        else {
            return Ok(());
        };

        let old = items.remove(index);
        self.store_leaf(pages, leaf, &items)?;
        if let StoredValue::Overflow { head, len } = old.value {
            retire_overflow(pages, head, len)?;
        }

        Ok(())
    }

    /// Cursor at the first key greater than or equal to `start`.
    ///
    /// [`None`] starts at the first key. The cursor does not allocate a page.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::KeyTooLarge`] when `start` is longer than
    /// [`MAX_KEY_LEN`]. Returns [`TreeError::Corrupt`] when a page on the path
    /// to the start key is invalid.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::btree::{BTree, PageWrite};
    ///
    /// fn scan<P: PageWrite>(pages: &mut P) -> Result<(), pizzlydb_storage::btree::TreeError> {
    ///     let mut tree = BTree::new();
    ///     tree.insert(pages, b"a", b"1")?;
    ///     tree.insert(pages, b"b", b"2")?;
    ///     let mut cursor = tree.scan_from(pages, Some(b"b"))?;
    ///     let entry = cursor.next(pages)?.unwrap();
    ///     assert_eq!(entry.key, b"b");
    ///     Ok(())
    /// }
    /// ```
    pub(crate) fn scan_from<P: PageRead>(
        &self,
        pages: &P,
        start: Option<&[u8]>,
    ) -> Result<Cursor, TreeError> {
        if let Some(bound) = start {
            check_key(bound)?;
        }

        if is_zero(self.root) {
            return Ok(Cursor::empty());
        }

        let bound = start.unwrap_or(b"");
        let leaf = if start.is_none() {
            leftmost(pages, self.root)?
        } else {
            find_leaf(pages, self.root, bound)?
        };

        let image = load_page(pages, leaf)?;
        ensure_leaf_order(&image, leaf)?;

        let fields = expect_leaf(&image, leaf)?;
        let (index, offset) = if start.is_none() {
            (0, BODY_OFF)
        } else {
            seek_leaf(&image, &fields, bound)?
        };

        Ok(Cursor {
            page_id: leaf,
            index,
            offset,
            image: Some(image),
            key_buf: Vec::new(),
            value_buf: Vec::new(),
            yielded: false,
            leaf_hops: 0,
        })
    }

    /// Check separators, order, links, occupancy, and overflow chains.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Corrupt`] when a check fails.
    pub(crate) fn verify<P: PageRead>(&self, pages: &P) -> Result<(), TreeError> {
        if is_zero(self.root) {
            return Ok(());
        }

        verify_node(pages, self.root, None, None, true, 0)?;

        let mut leaves = Vec::new();
        collect_leaves(pages, self.root, &mut leaves, 0)?;

        let mut id = leftmost(pages, self.root)?;
        let mut seen = 0usize;
        let mut hops = 0u32;
        while !is_zero(id) {
            hops = hops.checked_add(1).ok_or(TreeError::Corrupt)?;

            if hops > MAX_LEAF_HOPS {
                return Err(TreeError::Corrupt);
            }

            if leaves.get(seen) != Some(&id) {
                return Err(TreeError::Corrupt);
            }

            seen += 1;
            let image = load_page(pages, id)?;
            let fields = expect_leaf(&image, id)?;
            let right = fields.right;

            if !is_zero(right) {
                if right == id {
                    return Err(TreeError::Corrupt);
                }

                let next = load_page(pages, right)?;
                let next_fields = expect_leaf(&next, right)?;
                if next_fields.left != id {
                    return Err(TreeError::Corrupt);
                }
            }

            id = right;
        }

        if seen != leaves.len() {
            return Err(TreeError::Corrupt);
        }

        Ok(())
    }

    fn store_leaf<P: PageWrite>(
        &mut self,
        pages: &mut P,
        id: PageId,
        items: &[LeafItem],
    ) -> Result<(), TreeError> {
        if items.is_empty() {
            return self.unlink_leaf(pages, id);
        }

        self.commit_leaf(pages, id, items)?;
        self.rebalance(pages, id)
    }

    fn commit_leaf<P: PageWrite>(
        &mut self,
        pages: &mut P,
        id: PageId,
        items: &[LeafItem],
    ) -> Result<(), TreeError> {
        let payload = pack_leaf(items)?;
        let image = load_page(pages, id)?;
        let fields = expect_leaf(&image, id)?;

        if payload <= BODY_CAP {
            write_leaf(pages, id, fields.parent, fields.left, fields.right, items)?;
            self.propagate_min(pages, id)?;
            return Ok(());
        }

        let point = split_leaf_point(items)?;
        let right_id = pages.allocate(PageKind::BTreeLeaf)?;
        let separator = items[point].key.clone();

        write_leaf(
            pages,
            id,
            fields.parent,
            fields.left,
            right_id,
            &items[..point],
        )?;

        write_leaf(
            pages,
            right_id,
            fields.parent,
            id,
            fields.right,
            &items[point..],
        )?;

        if !is_zero(fields.right) {
            set_sibling(pages, fields.right, LEFT_OFF, right_id)?;
        }

        self.add_child(pages, fields.parent, id, right_id, &separator)
    }

    fn commit_internal<P: PageWrite>(
        &mut self,
        pages: &mut P,
        id: PageId,
        left_child: PageId,
        items: &[InternalItem],
    ) -> Result<(), TreeError> {
        let payload = pack_internal(items)?;

        if payload <= BODY_CAP {
            let image = load_page(pages, id)?;
            let fields = expect_internal(&image, id)?;
            return write_internal(pages, id, fields.parent, left_child, items);
        }

        let point = split_internal_point(items)?;
        let right_id = pages.allocate(PageKind::BTreeInternal)?;
        let image = load_page(pages, id)?;
        let fields = expect_internal(&image, id)?;
        let separator = items[point].key.clone();
        let right_left = items[point].child;

        write_internal(pages, id, fields.parent, left_child, &items[..point])?;
        write_internal(
            pages,
            right_id,
            fields.parent,
            right_left,
            &items[point + 1..],
        )?;

        self.add_child(pages, fields.parent, id, right_id, &separator)
    }

    fn add_child<P: PageWrite>(
        &mut self,
        pages: &mut P,
        parent: PageId,
        after: PageId,
        new_id: PageId,
        separator: &[u8],
    ) -> Result<(), TreeError> {
        if is_zero(parent) {
            let root_id = pages.allocate(PageKind::BTreeInternal)?;
            write_internal(
                pages,
                root_id,
                PageId::new(0),
                after,
                &[InternalItem {
                    key: separator.to_vec(),
                    child: new_id,
                }],
            )?;

            self.root = root_id;
            return Ok(());
        }

        let image = load_page(pages, parent)?;
        let fields = expect_internal(&image, parent)?;
        let mut items = read_internal_items(&image, &fields)?;

        let slot = child_slot(fields.left_child, &items, after)?;
        items.insert(
            slot,
            InternalItem {
                key: separator.to_vec(),
                child: new_id,
            },
        );

        self.commit_internal(pages, parent, fields.left_child, &items)
    }

    fn unlink_leaf<P: PageWrite>(&mut self, pages: &mut P, id: PageId) -> Result<(), TreeError> {
        let image = load_page(pages, id)?;
        let fields = expect_leaf(&image, id)?;

        if is_zero(fields.parent) {
            self.root = PageId::new(0);
            return pages.retire(id);
        }

        if !is_zero(fields.left) {
            set_sibling(pages, fields.left, RIGHT_OFF, fields.right)?;
        }

        if !is_zero(fields.right) {
            set_sibling(pages, fields.right, LEFT_OFF, fields.left)?;
        }

        self.drop_child(pages, fields.parent, id)?;
        pages.retire(id)?;
        self.rebalance(pages, fields.parent)
    }

    fn drop_child<P: PageWrite>(
        &mut self,
        pages: &mut P,
        parent: PageId,
        child: PageId,
    ) -> Result<(), TreeError> {
        let image = load_page(pages, parent)?;
        let fields = expect_internal(&image, parent)?;

        let mut items = read_internal_items(&image, &fields)?;
        let left_child = if fields.left_child == child {
            if items.is_empty() {
                return Err(TreeError::Corrupt);
            }
            items.remove(0).child
        } else {
            let Some(index) = items.iter().position(|item| item.child == child) else {
                return Err(TreeError::Corrupt);
            };
            items.remove(index);
            fields.left_child
        };

        write_internal(pages, parent, fields.parent, left_child, &items)?;
        self.propagate_min(pages, parent)
    }

    fn propagate_min<P: PageWrite>(
        &mut self,
        pages: &mut P,
        start: PageId,
    ) -> Result<(), TreeError> {
        let mut current = start;
        let mut hops = 0u32;

        loop {
            hops = hops.checked_add(1).ok_or(TreeError::Corrupt)?;
            if hops > MAX_TREE_HOPS || is_zero(current) {
                return Err(TreeError::Corrupt);
            }

            let image = load_page(pages, current)?;
            let fields = read_fields(&image)?;
            if fields.kind != PageKind::BTreeLeaf && fields.kind != PageKind::BTreeInternal {
                return Err(TreeError::Corrupt);
            }

            if is_zero(fields.parent) {
                return Ok(());
            }

            let parent_image = load_page(pages, fields.parent)?;
            let parent_fields = expect_internal(&parent_image, fields.parent)?;
            if parent_fields.left_child == current {
                current = fields.parent;
                continue;
            }

            let min = subtree_min(pages, current)?;
            let mut items = read_internal_items(&parent_image, &parent_fields)?;
            let Some(item) = items.iter_mut().find(|item| item.child == current) else {
                return Err(TreeError::Corrupt);
            };

            if item.key == min {
                return Ok(());
            }

            item.key = min;
            return self.commit_internal(pages, fields.parent, parent_fields.left_child, &items);
        }
    }

    fn rebalance<P: PageWrite>(&mut self, pages: &mut P, id: PageId) -> Result<(), TreeError> {
        if is_zero(id) {
            return Ok(());
        }

        let fields = load_fields(pages, id)?;

        if is_zero(fields.parent) {
            if fields.kind == PageKind::BTreeInternal && fields.count == 0 {
                self.collapse_root(pages, id, fields.left_child)?;
            }
            return Ok(());
        }

        if fields.payload as usize >= MIN_PAYLOAD {
            return Ok(());
        }

        let parent_fields = load_fields(pages, fields.parent)?;
        if parent_fields.count == 0 {
            self.rebalance(pages, fields.parent)?;
            let fields = load_fields(pages, id)?;
            if is_zero(fields.parent) || fields.payload as usize >= MIN_PAYLOAD {
                return Ok(());
            }
            return self.rebalance_siblings(pages, id, &fields);
        }

        self.rebalance_siblings(pages, id, &fields)
    }

    fn rebalance_siblings<P: PageWrite>(
        &mut self,
        pages: &mut P,
        id: PageId,
        fields: &Fields,
    ) -> Result<(), TreeError> {
        let (left, right) = match fields.kind {
            PageKind::BTreeLeaf => (fields.left, fields.right),
            PageKind::BTreeInternal => (
                sibling_child(pages, fields.parent, id, false)?,
                sibling_child(pages, fields.parent, id, true)?,
            ),
            _ => return Err(TreeError::Corrupt),
        };

        if !is_zero(right) && self.redistribute(pages, id, right)? {
            return Ok(());
        }

        if !is_zero(left) && self.redistribute(pages, left, id)? {
            return Ok(());
        }

        if !is_zero(right) && self.merge_nodes(pages, id, right)? {
            return Ok(());
        }

        if !is_zero(left) && self.merge_nodes(pages, left, id)? {
            return Ok(());
        }

        Ok(())
    }

    fn collapse_root<P: PageWrite>(
        &mut self,
        pages: &mut P,
        id: PageId,
        only: PageId,
    ) -> Result<(), TreeError> {
        if is_zero(only) {
            return Err(TreeError::Corrupt);
        }

        set_parent(pages, only, PageId::new(0))?;
        self.root = only;
        pages.retire(id)
    }

    fn redistribute<P: PageWrite>(
        &mut self,
        pages: &mut P,
        left: PageId,
        right: PageId,
    ) -> Result<bool, TreeError> {
        let left_fields = load_fields(pages, left)?;
        let right_fields = load_fields(pages, right)?;

        if left_fields.kind != right_fields.kind || left_fields.parent != right_fields.parent {
            return Err(TreeError::Corrupt);
        }

        if is_zero(left_fields.parent) {
            return Err(TreeError::Corrupt);
        }

        match left_fields.kind {
            PageKind::BTreeLeaf => {
                self.redistribute_leaves(pages, left, right, &left_fields, &right_fields)
            }
            PageKind::BTreeInternal => {
                self.redistribute_internal(pages, left, right, &left_fields, &right_fields)
            }
            _ => Err(TreeError::Corrupt),
        }
    }

    fn redistribute_leaves<P: PageWrite>(
        &mut self,
        pages: &mut P,
        left: PageId,
        right: PageId,
        left_fields: &Fields,
        right_fields: &Fields,
    ) -> Result<bool, TreeError> {
        if left_fields.right != right || right_fields.left != left {
            return Err(TreeError::Corrupt);
        }

        let left_image = load_page(pages, left)?;
        let right_image = load_page(pages, right)?;
        let mut combined = read_leaf_items(&left_image, left_fields)?;
        combined.extend(read_leaf_items(&right_image, right_fields)?);

        if pack_leaf(&combined)? <= BODY_CAP {
            return Ok(false);
        }

        let point = split_leaf_point(&combined)?;
        let left_bytes = pack_leaf(&combined[..point])?;
        let right_bytes = pack_leaf(&combined[point..])?;
        if left_bytes < MIN_PAYLOAD || right_bytes < MIN_PAYLOAD {
            return Ok(false);
        }

        write_leaf(
            pages,
            left,
            left_fields.parent,
            left_fields.left,
            right,
            &combined[..point],
        )?;

        write_leaf(
            pages,
            right,
            right_fields.parent,
            left,
            right_fields.right,
            &combined[point..],
        )?;

        self.propagate_min(pages, right)?;
        Ok(true)
    }

    fn redistribute_internal<P: PageWrite>(
        &mut self,
        pages: &mut P,
        left: PageId,
        right: PageId,
        left_fields: &Fields,
        right_fields: &Fields,
    ) -> Result<bool, TreeError> {
        let parent_image = load_page(pages, left_fields.parent)?;
        let parent_fields = expect_internal(&parent_image, left_fields.parent)?;
        let parent_items = read_internal_items(&parent_image, &parent_fields)?;

        let Some(separator) = parent_items
            .iter()
            .find(|item| item.child == right)
            .map(|item| item.key.clone())
        else {
            return Err(TreeError::Corrupt);
        };

        let left_image = load_page(pages, left)?;
        let right_image = load_page(pages, right)?;
        let mut combined = read_internal_items(&left_image, left_fields)?;

        // The parent separator is the minimum of the right node, so it becomes
        // the separator in front of that node's leftmost child.
        combined.push(InternalItem {
            key: separator,
            child: right_fields.left_child,
        });

        combined.extend(read_internal_items(&right_image, right_fields)?);

        if pack_internal(&combined)? <= BODY_CAP {
            return Ok(false);
        }

        let point = split_internal_point(&combined)?;
        let left_bytes = pack_internal(&combined[..point])?;
        let right_bytes = pack_internal(&combined[point + 1..])?;
        if left_bytes < MIN_PAYLOAD || right_bytes < MIN_PAYLOAD {
            return Ok(false);
        }

        let right_left = combined[point].child;

        write_internal(
            pages,
            left,
            left_fields.parent,
            left_fields.left_child,
            &combined[..point],
        )?;

        write_internal(
            pages,
            right,
            right_fields.parent,
            right_left,
            &combined[point + 1..],
        )?;

        self.propagate_min(pages, right)?;
        Ok(true)
    }

    fn merge_nodes<P: PageWrite>(
        &mut self,
        pages: &mut P,
        left: PageId,
        right: PageId,
    ) -> Result<bool, TreeError> {
        let left_fields = load_fields(pages, left)?;
        let right_fields = load_fields(pages, right)?;

        if left_fields.kind != right_fields.kind || left_fields.parent != right_fields.parent {
            return Err(TreeError::Corrupt);
        }

        match left_fields.kind {
            PageKind::BTreeLeaf => {
                self.merge_leaves(pages, left, right, &left_fields, &right_fields)
            }
            PageKind::BTreeInternal => {
                self.merge_internal(pages, left, right, &left_fields, &right_fields)
            }
            _ => Err(TreeError::Corrupt),
        }
    }

    fn merge_leaves<P: PageWrite>(
        &mut self,
        pages: &mut P,
        left: PageId,
        right: PageId,
        left_fields: &Fields,
        right_fields: &Fields,
    ) -> Result<bool, TreeError> {
        if left_fields.right != right || right_fields.left != left {
            return Err(TreeError::Corrupt);
        }

        let left_image = load_page(pages, left)?;
        let right_image = load_page(pages, right)?;
        let mut combined = read_leaf_items(&left_image, left_fields)?;
        combined.extend(read_leaf_items(&right_image, right_fields)?);

        if pack_leaf(&combined)? > BODY_CAP {
            return Ok(false);
        }

        write_leaf(
            pages,
            left,
            left_fields.parent,
            left_fields.left,
            right_fields.right,
            &combined,
        )?;

        if !is_zero(right_fields.right) {
            set_sibling(pages, right_fields.right, LEFT_OFF, left)?;
        }

        self.drop_child(pages, left_fields.parent, right)?;
        pages.retire(right)?;
        self.propagate_min(pages, left)?;
        self.rebalance(pages, left_fields.parent)?;

        Ok(true)
    }

    fn merge_internal<P: PageWrite>(
        &mut self,
        pages: &mut P,
        left: PageId,
        right: PageId,
        left_fields: &Fields,
        right_fields: &Fields,
    ) -> Result<bool, TreeError> {
        let parent_image = load_page(pages, left_fields.parent)?;
        let parent_fields = expect_internal(&parent_image, left_fields.parent)?;
        let parent_items = read_internal_items(&parent_image, &parent_fields)?;

        let Some(separator) = parent_items
            .iter()
            .find(|item| item.child == right)
            .map(|item| item.key.clone())
        else {
            return Err(TreeError::Corrupt);
        };

        let left_image = load_page(pages, left)?;
        let right_image = load_page(pages, right)?;
        let mut combined = read_internal_items(&left_image, left_fields)?;
        combined.push(InternalItem {
            key: separator,
            child: right_fields.left_child,
        });
        combined.extend(read_internal_items(&right_image, right_fields)?);

        if pack_internal(&combined)? > BODY_CAP {
            return Ok(false);
        }

        write_internal(
            pages,
            left,
            left_fields.parent,
            left_fields.left_child,
            &combined,
        )?;

        self.drop_child(pages, left_fields.parent, right)?;
        pages.retire(right)?;
        self.rebalance(pages, left_fields.parent)?;

        Ok(true)
    }
}

/// Key and value borrowed from a [`Cursor`].
///
/// Both slices end at the next call to [`Cursor::next`].
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::{BTree, PageWrite};
///
/// fn first_key<P: PageWrite>(pages: &mut P) -> Result<(), pizzlydb_storage::btree::TreeError> {
///     let mut tree = BTree::new();
///     tree.insert(pages, b"k", b"v")?;
///     let mut cursor = tree.scan_from(pages, None)?;
///     let entry = cursor.next(pages)?.unwrap();
///     assert_eq!(entry.key, b"k");
///     assert_eq!(entry.value, b"v");
///     Ok(())
/// }
/// ```
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry<'a> {
    /// Key at the cursor.
    pub(crate) key: &'a [u8],
    /// Value at the cursor.
    pub(crate) value: &'a [u8],
}

/// Forward scan over one tree.
///
/// This type does not implement [`Copy`] or [`Clone`]. [`Cursor::next`] takes
/// `&mut self`, so a previous [`Entry`] cannot be held across another step.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::btree::{BTree, PageRead};
///
/// fn empty<P: PageRead>(pages: &P) -> Result<(), pizzlydb_storage::btree::TreeError> {
///     let tree = BTree::new();
///     let mut cursor = tree.scan_from(pages, None)?;
///     assert!(cursor.next(pages)?.is_none());
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub(crate) struct Cursor {
    page_id: PageId,
    index: u16,
    offset: usize,
    image: Option<[u8; PAGE_SIZE]>,
    key_buf: Vec<u8>,
    value_buf: Vec<u8>,
    yielded: bool,
    leaf_hops: u32,
}

impl Cursor {
    fn empty() -> Self {
        Self {
            page_id: PageId::new(0),
            index: 0,
            offset: BODY_OFF,
            image: None,
            key_buf: Vec::new(),
            value_buf: Vec::new(),
            yielded: false,
            leaf_hops: 0,
        }
    }

    /// Next key and value, or [`None`] at the end of the scan.
    ///
    /// The returned slices borrow this cursor. The scan past the last key does
    /// not allocate a page or a buffer.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Corrupt`] when a sibling link, key order, or
    /// overflow chain is invalid.
    pub(crate) fn next<'a, P: PageRead>(
        &'a mut self,
        pages: &P,
    ) -> Result<Option<Entry<'a>>, TreeError> {
        loop {
            if is_zero(self.page_id) {
                return Ok(None);
            }

            if self.image.is_none() {
                let image = load_page(pages, self.page_id)?;
                ensure_leaf_order(&image, self.page_id)?;
                self.offset = BODY_OFF;
                self.index = 0;
                self.image = Some(image);
            }

            let image = self.image.ok_or(TreeError::Corrupt)?;
            let fields = expect_leaf(&image, self.page_id)?;
            let payload_end = BODY_OFF + fields.payload as usize;

            if self.offset < payload_end && self.index < fields.count {
                let parsed = parse_leaf_entry(&image, self.offset, payload_end)?;
                let mut key_tmp = [0u8; MAX_KEY_LEN];
                let key_len = parsed.key.len();
                key_tmp[..key_len].copy_from_slice(parsed.key);

                if self.yielded && cmp_bytes(&self.key_buf, &key_tmp[..key_len]) != Ordering::Less {
                    return Err(TreeError::Corrupt);
                }

                let located = parsed.owned;
                self.fill_entry(&image, &key_tmp[..key_len], &located, pages)?;

                let next_offset = parsed.next;
                let next_index = self.index.checked_add(1).ok_or(TreeError::Corrupt)?;
                self.offset = next_offset;
                self.index = next_index;
                self.yielded = true;

                return Ok(Some(Entry {
                    key: &self.key_buf,
                    value: &self.value_buf,
                }));
            }

            let right = fields.right;
            if right == self.page_id {
                return Err(TreeError::Corrupt);
            }

            if is_zero(right) {
                self.page_id = PageId::new(0);
                self.image = None;
                return Ok(None);
            }

            self.leaf_hops = self.leaf_hops.checked_add(1).ok_or(TreeError::Corrupt)?;
            if self.leaf_hops > MAX_LEAF_HOPS {
                return Err(TreeError::Corrupt);
            }

            let previous = self.page_id;
            let next_image = load_page(pages, right)?;
            let next_fields = expect_leaf(&next_image, right)?;
            if next_fields.left != previous {
                return Err(TreeError::Corrupt);
            }

            ensure_leaf_order(&next_image, right)?;

            self.page_id = right;
            self.index = 0;
            self.offset = BODY_OFF;
            self.image = Some(next_image);
        }
    }

    fn fill_entry<P: PageRead>(
        &mut self,
        image: &[u8; PAGE_SIZE],
        key: &[u8],
        located: &OwnedValue,
        pages: &P,
    ) -> Result<(), TreeError> {
        self.key_buf.clear();
        self.key_buf.extend_from_slice(key);

        self.value_buf.clear();
        match *located {
            OwnedValue::Inline { at, len } => {
                let bytes = image.get(at..at + len).ok_or(TreeError::Corrupt)?;
                self.value_buf.extend_from_slice(bytes);
            }
            OwnedValue::Overflow { head, len } => {
                read_overflow_into(pages, head, len, &mut self.value_buf)?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
struct LeafItem {
    key: Vec<u8>,
    value: StoredValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StoredValue {
    Inline(Vec<u8>),
    Overflow { head: PageId, len: u64 },
}

#[derive(Clone, Debug)]
struct InternalItem {
    key: Vec<u8>,
    child: PageId,
}

#[derive(Clone, Copy, Debug)]
struct Fields {
    kind: PageKind,
    parent: PageId,
    left: PageId,
    right: PageId,
    left_child: PageId,
    count: u16,
    payload: u16,
}

struct LeafParsed<'a> {
    key: &'a [u8],
    owned: OwnedValue,
    next: usize,
}

#[derive(Clone, Copy, Debug)]
enum OwnedValue {
    Inline { at: usize, len: usize },
    Overflow { head: PageId, len: u64 },
}

fn cmp_bytes(left: &[u8], right: &[u8]) -> Ordering {
    left.cmp(right)
}

fn is_zero(id: PageId) -> bool {
    id.as_u64() == 0
}

fn check_key(key: &[u8]) -> Result<(), TreeError> {
    if key.len() > MAX_KEY_LEN {
        Err(TreeError::KeyTooLarge)
    } else {
        Ok(())
    }
}

fn check_value(value: &[u8]) -> Result<(), TreeError> {
    if value.len() > MAX_VALUE_LEN {
        Err(TreeError::ValueTooLarge)
    } else {
        Ok(())
    }
}

fn checked_header(image: &[u8], id: PageId) -> Result<PageHeader, TreeError> {
    let header = PageHeader::decode(image).map_err(|_| TreeError::Corrupt)?;
    if header.page_id() != id {
        return Err(TreeError::Corrupt);
    }
    Ok(header)
}

fn load_page<P: PageRead>(pages: &P, id: PageId) -> Result<[u8; PAGE_SIZE], TreeError> {
    if is_zero(id) {
        return Err(TreeError::Corrupt);
    }

    let pin = pages.pin(id)?;
    if pin.len() != PAGE_SIZE {
        return Err(TreeError::Corrupt);
    }

    let mut image = [0u8; PAGE_SIZE];
    image.copy_from_slice(&pin);
    drop(pin);

    checked_header(&image, id)?;
    Ok(image)
}

fn store_page<P: PageWrite>(
    pages: &mut P,
    id: PageId,
    image: &[u8; PAGE_SIZE],
) -> Result<(), TreeError> {
    let mut pin = pages.pin_mut(id)?;
    if pin.len() != PAGE_SIZE {
        return Err(TreeError::Corrupt);
    }
    pin.copy_from_slice(image);
    Ok(())
}

fn read_u16(image: &[u8], offset: usize) -> Result<u16, TreeError> {
    get_u16(image, offset).map_err(|_| TreeError::Corrupt)
}

fn read_u32(image: &[u8], offset: usize) -> Result<u32, TreeError> {
    get_u32(image, offset).map_err(|_| TreeError::Corrupt)
}

fn read_u64(image: &[u8], offset: usize) -> Result<u64, TreeError> {
    get_u64(image, offset).map_err(|_| TreeError::Corrupt)
}

fn write_u16(image: &mut [u8], offset: usize, value: u16) -> Result<(), TreeError> {
    put_u16(image, offset, value).map_err(|_| TreeError::Corrupt)
}

fn write_u32(image: &mut [u8], offset: usize, value: u32) -> Result<(), TreeError> {
    put_u32(image, offset, value).map_err(|_| TreeError::Corrupt)
}

fn write_u64(image: &mut [u8], offset: usize, value: u64) -> Result<(), TreeError> {
    put_u64(image, offset, value).map_err(|_| TreeError::Corrupt)
}

fn read_fields(image: &[u8]) -> Result<Fields, TreeError> {
    let header = PageHeader::decode(image).map_err(|_| TreeError::Corrupt)?;
    if read_u32(image, RESERVED_OFF)? != 0 {
        return Err(TreeError::Corrupt);
    }

    let payload = read_u16(image, PAYLOAD_OFF)?;
    if payload as usize > BODY_CAP {
        return Err(TreeError::Corrupt);
    }

    Ok(Fields {
        kind: header.kind(),
        parent: PageId::new(read_u64(image, PARENT_OFF)?),
        left: PageId::new(read_u64(image, LEFT_OFF)?),
        right: PageId::new(read_u64(image, RIGHT_OFF)?),
        left_child: PageId::new(read_u64(image, LEFT_CHILD_OFF)?),
        count: read_u16(image, COUNT_OFF)?,
        payload,
    })
}

fn load_fields<P: PageRead>(pages: &P, id: PageId) -> Result<Fields, TreeError> {
    let image = load_page(pages, id)?;
    let fields = read_fields(&image)?;

    if fields.kind != PageKind::BTreeLeaf && fields.kind != PageKind::BTreeInternal {
        return Err(TreeError::Corrupt);
    }

    Ok(fields)
}

fn expect_leaf(image: &[u8], id: PageId) -> Result<Fields, TreeError> {
    let fields = read_fields(image)?;
    if fields.kind != PageKind::BTreeLeaf || !is_zero(fields.left_child) {
        return Err(TreeError::Corrupt);
    }

    checked_header(image, id)?;
    Ok(fields)
}

fn expect_internal(image: &[u8], id: PageId) -> Result<Fields, TreeError> {
    let fields = read_fields(image)?;
    if fields.kind != PageKind::BTreeInternal || !is_zero(fields.left) || !is_zero(fields.right) {
        return Err(TreeError::Corrupt);
    }

    checked_header(image, id)?;
    Ok(fields)
}

fn seal_image(
    image: &mut [u8; PAGE_SIZE],
    kind: PageKind,
    id: PageId,
    lsn: Lsn,
) -> Result<(), TreeError> {
    PageHeader::new(kind, id, lsn)
        .encode(image)
        .map_err(|_| TreeError::Corrupt)
}

fn write_leaf<P: PageWrite>(
    pages: &mut P,
    id: PageId,
    parent: PageId,
    left: PageId,
    right: PageId,
    items: &[LeafItem],
) -> Result<(), TreeError> {
    let existing = load_page(pages, id)?;
    let header = checked_header(&existing, id)?;
    if header.kind() != PageKind::BTreeLeaf {
        return Err(TreeError::Corrupt);
    }

    ensure_increasing(items.iter().map(|item| item.key.as_slice()))?;

    let payload = pack_leaf(items)?;
    if payload > BODY_CAP {
        return Err(TreeError::Corrupt);
    }

    let mut image = [0u8; PAGE_SIZE];
    encode_leaf(&mut image, parent, left, right, items, payload)?;
    seal_image(&mut image, PageKind::BTreeLeaf, id, header.lsn())?;
    store_page(pages, id, &image)
}

fn write_internal<P: PageWrite>(
    pages: &mut P,
    id: PageId,
    parent: PageId,
    left_child: PageId,
    items: &[InternalItem],
) -> Result<(), TreeError> {
    if is_zero(left_child) {
        return Err(TreeError::Corrupt);
    }

    let existing = load_page(pages, id)?;
    let header = checked_header(&existing, id)?;
    if header.kind() != PageKind::BTreeInternal {
        return Err(TreeError::Corrupt);
    }

    ensure_increasing(items.iter().map(|item| item.key.as_slice()))?;
    for item in items {
        if is_zero(item.child) {
            return Err(TreeError::Corrupt);
        }
    }

    let payload = pack_internal(items)?;
    if payload > BODY_CAP {
        return Err(TreeError::Corrupt);
    }

    let mut image = [0u8; PAGE_SIZE];
    encode_internal(&mut image, parent, left_child, items, payload)?;
    seal_image(&mut image, PageKind::BTreeInternal, id, header.lsn())?;
    store_page(pages, id, &image)?;

    set_parent(pages, left_child, id)?;
    for item in items {
        set_parent(pages, item.child, id)?;
    }

    Ok(())
}

fn encode_leaf(
    image: &mut [u8; PAGE_SIZE],
    parent: PageId,
    left: PageId,
    right: PageId,
    items: &[LeafItem],
    payload: usize,
) -> Result<(), TreeError> {
    write_u64(image, RIGHT_OFF, right.as_u64())?;
    write_u64(image, LEFT_OFF, left.as_u64())?;
    write_u64(image, PARENT_OFF, parent.as_u64())?;
    write_u64(image, LEFT_CHILD_OFF, 0)?;
    write_u16(
        image,
        COUNT_OFF,
        u16::try_from(items.len()).map_err(|_| TreeError::Corrupt)?,
    )?;
    write_u16(
        image,
        PAYLOAD_OFF,
        u16::try_from(payload).map_err(|_| TreeError::Corrupt)?,
    )?;
    write_u32(image, RESERVED_OFF, 0)?;

    let mut offset = BODY_OFF;
    for item in items {
        offset = encode_leaf_entry(image, offset, item)?;
    }

    if offset != BODY_OFF + payload {
        return Err(TreeError::Corrupt);
    }

    Ok(())
}

fn encode_leaf_entry(image: &mut [u8], offset: usize, item: &LeafItem) -> Result<usize, TreeError> {
    if item.key.len() > MAX_KEY_LEN {
        return Err(TreeError::Corrupt);
    }

    write_u16(
        image,
        offset,
        u16::try_from(item.key.len()).map_err(|_| TreeError::Corrupt)?,
    )?;

    let flag = match &item.value {
        StoredValue::Inline(_) => FLAG_INLINE,
        StoredValue::Overflow { .. } => FLAG_OVERFLOW,
    };
    let flag_at = offset.checked_add(2).ok_or(TreeError::Corrupt)?;
    *image.get_mut(flag_at).ok_or(TreeError::Corrupt)? = flag;
    *image.get_mut(flag_at + 1).ok_or(TreeError::Corrupt)? = 0;

    let mut cursor = offset.checked_add(4).ok_or(TreeError::Corrupt)?;
    let key_end = cursor
        .checked_add(item.key.len())
        .ok_or(TreeError::Corrupt)?;

    image
        .get_mut(cursor..key_end)
        .ok_or(TreeError::Corrupt)?
        .copy_from_slice(&item.key);

    cursor = key_end;
    match &item.value {
        StoredValue::Inline(value) => {
            write_u32(
                image,
                cursor,
                u32::try_from(value.len()).map_err(|_| TreeError::Corrupt)?,
            )?;
            cursor = cursor.checked_add(4).ok_or(TreeError::Corrupt)?;
            let end = cursor.checked_add(value.len()).ok_or(TreeError::Corrupt)?;
            image
                .get_mut(cursor..end)
                .ok_or(TreeError::Corrupt)?
                .copy_from_slice(value);
            cursor = end;
        }
        StoredValue::Overflow { head, len } => {
            write_u64(image, cursor, head.as_u64())?;
            write_u64(image, cursor + 8, *len)?;
            cursor = cursor.checked_add(16).ok_or(TreeError::Corrupt)?;
        }
    }

    Ok(cursor)
}

fn encode_internal(
    image: &mut [u8; PAGE_SIZE],
    parent: PageId,
    left_child: PageId,
    items: &[InternalItem],
    payload: usize,
) -> Result<(), TreeError> {
    write_u64(image, RIGHT_OFF, 0)?;
    write_u64(image, LEFT_OFF, 0)?;
    write_u64(image, PARENT_OFF, parent.as_u64())?;
    write_u64(image, LEFT_CHILD_OFF, left_child.as_u64())?;
    write_u16(
        image,
        COUNT_OFF,
        u16::try_from(items.len()).map_err(|_| TreeError::Corrupt)?,
    )?;
    write_u16(
        image,
        PAYLOAD_OFF,
        u16::try_from(payload).map_err(|_| TreeError::Corrupt)?,
    )?;
    write_u32(image, RESERVED_OFF, 0)?;

    let mut offset = BODY_OFF;
    for item in items {
        if item.key.len() > MAX_KEY_LEN {
            return Err(TreeError::Corrupt);
        }

        write_u16(
            image,
            offset,
            u16::try_from(item.key.len()).map_err(|_| TreeError::Corrupt)?,
        )?;
        write_u16(image, offset + 2, 0)?;

        let mut cursor = offset.checked_add(4).ok_or(TreeError::Corrupt)?;
        let key_end = cursor
            .checked_add(item.key.len())
            .ok_or(TreeError::Corrupt)?;

        image
            .get_mut(cursor..key_end)
            .ok_or(TreeError::Corrupt)?
            .copy_from_slice(&item.key);

        cursor = key_end;
        write_u64(image, cursor, item.child.as_u64())?;
        offset = cursor.checked_add(8).ok_or(TreeError::Corrupt)?;
    }

    if offset != BODY_OFF + payload {
        return Err(TreeError::Corrupt);
    }

    Ok(())
}

fn pack_leaf(items: &[LeafItem]) -> Result<usize, TreeError> {
    let mut total = 0usize;
    for item in items {
        let extra = match &item.value {
            StoredValue::Inline(value) => LEAF_INLINE_OVERHEAD
                .checked_add(item.key.len())
                .and_then(|sum| sum.checked_add(value.len())),
            StoredValue::Overflow { .. } => LEAF_OVERFLOW_OVERHEAD.checked_add(item.key.len()),
        }
        .ok_or(TreeError::Corrupt)?;

        total = total.checked_add(extra).ok_or(TreeError::Corrupt)?;
    }

    Ok(total)
}

fn pack_internal(items: &[InternalItem]) -> Result<usize, TreeError> {
    let mut total = 0usize;
    for item in items {
        let extra = INTERNAL_OVERHEAD
            .checked_add(item.key.len())
            .ok_or(TreeError::Corrupt)?;

        total = total.checked_add(extra).ok_or(TreeError::Corrupt)?;
    }

    Ok(total)
}

fn inline_len(key_len: usize, value_len: usize) -> Option<usize> {
    LEAF_INLINE_OVERHEAD
        .checked_add(key_len)
        .and_then(|sum| sum.checked_add(value_len))
}

fn read_leaf_items(image: &[u8], fields: &Fields) -> Result<Vec<LeafItem>, TreeError> {
    let end = BODY_OFF + fields.payload as usize;
    let mut offset = BODY_OFF;
    let mut items = Vec::with_capacity(fields.count as usize);
    let mut previous: Option<Vec<u8>> = None;

    for _ in 0..fields.count {
        let parsed = parse_leaf_entry(image, offset, end)?;
        if let Some(prior) = &previous
            && cmp_bytes(prior, parsed.key) != Ordering::Less
        {
            return Err(TreeError::Corrupt);
        }

        previous = Some(parsed.key.to_vec());
        let value = match parsed.owned {
            OwnedValue::Inline { at, len } => {
                let bytes = image.get(at..at + len).ok_or(TreeError::Corrupt)?;
                StoredValue::Inline(bytes.to_vec())
            }
            OwnedValue::Overflow { head, len } => StoredValue::Overflow { head, len },
        };

        items.push(LeafItem {
            key: parsed.key.to_vec(),
            value,
        });

        offset = parsed.next;
    }

    if offset != end {
        return Err(TreeError::Corrupt);
    }

    Ok(items)
}

fn read_internal_items(image: &[u8], fields: &Fields) -> Result<Vec<InternalItem>, TreeError> {
    let end = BODY_OFF + fields.payload as usize;
    let mut offset = BODY_OFF;
    let mut items = Vec::with_capacity(fields.count as usize);
    let mut previous: Option<Vec<u8>> = None;

    for _ in 0..fields.count {
        if offset
            .checked_add(INTERNAL_OVERHEAD)
            .ok_or(TreeError::Corrupt)?
            > end
        {
            return Err(TreeError::Corrupt);
        }

        let key_len = read_u16(image, offset)? as usize;
        if key_len > MAX_KEY_LEN || read_u16(image, offset + 2)? != 0 {
            return Err(TreeError::Corrupt);
        }

        let key_at = offset.checked_add(4).ok_or(TreeError::Corrupt)?;
        let key_end = key_at.checked_add(key_len).ok_or(TreeError::Corrupt)?;
        let child_at = key_end.checked_add(8).ok_or(TreeError::Corrupt)?;
        if child_at > end {
            return Err(TreeError::Corrupt);
        }

        let key = image.get(key_at..key_end).ok_or(TreeError::Corrupt)?;
        if let Some(prior) = &previous
            && cmp_bytes(prior, key) != Ordering::Less
        {
            return Err(TreeError::Corrupt);
        }

        let child = PageId::new(read_u64(image, key_end)?);
        if is_zero(child) {
            return Err(TreeError::Corrupt);
        }

        previous = Some(key.to_vec());
        items.push(InternalItem {
            key: key.to_vec(),
            child,
        });

        offset = child_at;
    }

    if offset != end {
        return Err(TreeError::Corrupt);
    }

    Ok(items)
}

fn parse_leaf_entry(image: &[u8], offset: usize, end: usize) -> Result<LeafParsed<'_>, TreeError> {
    if offset.checked_add(4).ok_or(TreeError::Corrupt)? > end {
        return Err(TreeError::Corrupt);
    }

    let key_len = read_u16(image, offset)? as usize;
    if key_len > MAX_KEY_LEN {
        return Err(TreeError::Corrupt);
    }

    let flag = *image.get(offset + 2).ok_or(TreeError::Corrupt)?;
    if image.get(offset + 3).ok_or(TreeError::Corrupt)? != &0 {
        return Err(TreeError::Corrupt);
    }

    let key_at = offset + 4;
    let key_end = key_at.checked_add(key_len).ok_or(TreeError::Corrupt)?;
    if key_end > end {
        return Err(TreeError::Corrupt);
    }

    let key = image.get(key_at..key_end).ok_or(TreeError::Corrupt)?;
    let (owned, next) = match flag {
        FLAG_INLINE => {
            if key_end.checked_add(4).ok_or(TreeError::Corrupt)? > end {
                return Err(TreeError::Corrupt);
            }

            let len = read_u32(image, key_end)? as usize;
            let at = key_end + 4;
            let value_end = at.checked_add(len).ok_or(TreeError::Corrupt)?;
            if value_end > end {
                return Err(TreeError::Corrupt);
            }

            (OwnedValue::Inline { at, len }, value_end)
        }
        FLAG_OVERFLOW => {
            if key_end.checked_add(16).ok_or(TreeError::Corrupt)? > end {
                return Err(TreeError::Corrupt);
            }

            let head = PageId::new(read_u64(image, key_end)?);
            let len = read_u64(image, key_end + 8)?;
            if is_zero(head) || len == 0 || len > MAX_VALUE_LEN as u64 {
                return Err(TreeError::Corrupt);
            }

            (OwnedValue::Overflow { head, len }, key_end + 16)
        }
        _ => return Err(TreeError::Corrupt),
    };

    Ok(LeafParsed { key, owned, next })
}

fn ensure_leaf_order(image: &[u8], id: PageId) -> Result<(), TreeError> {
    let fields = expect_leaf(image, id)?;
    let end = BODY_OFF + fields.payload as usize;
    let mut offset = BODY_OFF;
    let mut previous = [0u8; MAX_KEY_LEN];
    let mut previous_len: Option<usize> = None;

    for _ in 0..fields.count {
        let parsed = parse_leaf_entry(image, offset, end)?;
        if let Some(len) = previous_len
            && cmp_bytes(&previous[..len], parsed.key) != Ordering::Less
        {
            return Err(TreeError::Corrupt);
        }

        previous[..parsed.key.len()].copy_from_slice(parsed.key);
        previous_len = Some(parsed.key.len());
        offset = parsed.next;
    }

    if offset != end {
        return Err(TreeError::Corrupt);
    }

    Ok(())
}

fn seek_leaf(image: &[u8], fields: &Fields, bound: &[u8]) -> Result<(u16, usize), TreeError> {
    let end = BODY_OFF + fields.payload as usize;
    let mut offset = BODY_OFF;
    let mut index = 0u16;

    while index < fields.count {
        let parsed = parse_leaf_entry(image, offset, end)?;
        if cmp_bytes(parsed.key, bound) != Ordering::Less {
            return Ok((index, offset));
        }

        offset = parsed.next;
        index = index.checked_add(1).ok_or(TreeError::Corrupt)?;
    }

    Ok((index, offset))
}

fn ensure_increasing<'a>(keys: impl Iterator<Item = &'a [u8]>) -> Result<(), TreeError> {
    let mut previous: Option<&[u8]> = None;

    for key in keys {
        if let Some(prior) = previous
            && cmp_bytes(prior, key) != Ordering::Less
        {
            return Err(TreeError::Corrupt);
        }

        previous = Some(key);
    }

    Ok(())
}

fn make_stored<P: PageWrite>(
    pages: &mut P,
    key: &[u8],
    value: &[u8],
) -> Result<StoredValue, TreeError> {
    let inline = inline_len(key.len(), value.len()).ok_or(TreeError::Corrupt)?;

    if inline <= BODY_CAP {
        Ok(StoredValue::Inline(value.to_vec()))
    } else {
        Ok(StoredValue::Overflow {
            head: write_overflow(pages, value)?,
            len: u64::try_from(value.len()).map_err(|_| TreeError::Corrupt)?,
        })
    }
}

fn values_equal<P: PageRead>(
    pages: &P,
    stored: &StoredValue,
    value: &[u8],
) -> Result<bool, TreeError> {
    match stored {
        StoredValue::Inline(existing) => Ok(existing.as_slice() == value),
        StoredValue::Overflow { len, .. } if *len != value.len() as u64 => Ok(false),
        StoredValue::Overflow { head, len } => {
            let existing = materialize(
                pages,
                &StoredValue::Overflow {
                    head: *head,
                    len: *len,
                },
            )?;
            Ok(existing == value)
        }
    }
}

fn materialize<P: PageRead>(pages: &P, stored: &StoredValue) -> Result<Vec<u8>, TreeError> {
    match stored {
        StoredValue::Inline(value) => Ok(value.clone()),
        StoredValue::Overflow { head, len } => {
            let mut out = Vec::new();
            read_overflow_into(pages, *head, *len, &mut out)?;
            Ok(out)
        }
    }
}

fn upsert(items: &mut Vec<LeafItem>, key: &[u8], value: StoredValue) -> Option<StoredValue> {
    match items.binary_search_by(|item| cmp_bytes(&item.key, key)) {
        Ok(index) => Some(std::mem::replace(&mut items[index].value, value)),
        Err(index) => {
            items.insert(
                index,
                LeafItem {
                    key: key.to_vec(),
                    value,
                },
            );
            None
        }
    }
}

fn split_leaf_point(items: &[LeafItem]) -> Result<usize, TreeError> {
    if items.len() < 2 {
        return Err(TreeError::Corrupt);
    }

    let total = pack_leaf(items)?;
    let mut balanced: Option<usize> = None;
    let mut fallback: Option<usize> = None;
    let mut best_gap = usize::MAX;
    let mut fallback_gap = usize::MAX;

    for index in 1..items.len() {
        let left = pack_leaf(&items[..index])?;
        let right = total.checked_sub(left).ok_or(TreeError::Corrupt)?;
        if left > BODY_CAP || right > BODY_CAP {
            continue;
        }

        let gap = left.abs_diff(right);
        if gap < fallback_gap {
            fallback = Some(index);
            fallback_gap = gap;
        }

        if left >= MIN_PAYLOAD && right >= MIN_PAYLOAD && gap < best_gap {
            balanced = Some(index);
            best_gap = gap;
        }
    }

    balanced.or(fallback).ok_or(TreeError::Corrupt)
}

fn split_internal_point(items: &[InternalItem]) -> Result<usize, TreeError> {
    if items.is_empty() {
        return Err(TreeError::Corrupt);
    }

    let mut balanced: Option<usize> = None;
    let mut fallback: Option<usize> = None;
    let mut best_gap = usize::MAX;
    let mut fallback_gap = usize::MAX;

    for point in 0..items.len() {
        let left = pack_internal(&items[..point])?;
        let right = pack_internal(&items[point + 1..])?;
        if left > BODY_CAP || right > BODY_CAP {
            continue;
        }

        let gap = left.abs_diff(right);
        if gap < fallback_gap {
            fallback = Some(point);
            fallback_gap = gap;
        }

        if left >= MIN_PAYLOAD && right >= MIN_PAYLOAD && gap < best_gap {
            balanced = Some(point);
            best_gap = gap;
        }
    }

    balanced.or(fallback).ok_or(TreeError::Corrupt)
}

fn child_slot(
    left_child: PageId,
    items: &[InternalItem],
    after: PageId,
) -> Result<usize, TreeError> {
    if after == left_child {
        return Ok(0);
    }

    for (index, item) in items.iter().enumerate() {
        if item.child == after {
            return Ok(index + 1);
        }
    }

    Err(TreeError::Corrupt)
}

fn find_leaf<P: PageRead>(pages: &P, root: PageId, key: &[u8]) -> Result<PageId, TreeError> {
    let mut id = root;
    let mut hops = 0u32;

    loop {
        hops = hops.checked_add(1).ok_or(TreeError::Corrupt)?;
        if hops > MAX_TREE_HOPS || is_zero(id) {
            return Err(TreeError::Corrupt);
        }

        let image = load_page(pages, id)?;
        let fields = read_fields(&image)?;
        match fields.kind {
            PageKind::BTreeLeaf => {
                expect_leaf(&image, id)?;
                return Ok(id);
            }
            PageKind::BTreeInternal => {
                expect_internal(&image, id)?;
                id = choose_child(&image, &fields, key)?;
            }
            _ => return Err(TreeError::Corrupt),
        }
    }
}

fn choose_child(image: &[u8], fields: &Fields, key: &[u8]) -> Result<PageId, TreeError> {
    let items = read_internal_items(image, fields)?;
    if is_zero(fields.left_child) {
        return Err(TreeError::Corrupt);
    }

    let mut child = fields.left_child;
    for item in &items {
        if cmp_bytes(key, &item.key) == Ordering::Less {
            break;
        }

        if is_zero(item.child) {
            return Err(TreeError::Corrupt);
        }

        child = item.child;
    }

    Ok(child)
}

fn leftmost<P: PageRead>(pages: &P, root: PageId) -> Result<PageId, TreeError> {
    let mut id = root;
    let mut hops = 0u32;

    loop {
        hops = hops.checked_add(1).ok_or(TreeError::Corrupt)?;
        if hops > MAX_TREE_HOPS || is_zero(id) {
            return Err(TreeError::Corrupt);
        }

        let image = load_page(pages, id)?;
        let fields = read_fields(&image)?;
        match fields.kind {
            PageKind::BTreeLeaf => return Ok(id),
            PageKind::BTreeInternal => {
                if is_zero(fields.left_child) {
                    return Err(TreeError::Corrupt);
                }
                id = fields.left_child;
            }
            _ => return Err(TreeError::Corrupt),
        }
    }
}

fn subtree_min<P: PageRead>(pages: &P, start: PageId) -> Result<Vec<u8>, TreeError> {
    let leaf = leftmost(pages, start)?;
    let image = load_page(pages, leaf)?;
    let fields = expect_leaf(&image, leaf)?;
    if fields.count == 0 {
        return Err(TreeError::Corrupt);
    }

    let parsed = parse_leaf_entry(&image, BODY_OFF, BODY_OFF + fields.payload as usize)?;
    Ok(parsed.key.to_vec())
}

fn sibling_child<P: PageRead>(
    pages: &P,
    parent: PageId,
    child: PageId,
    right: bool,
) -> Result<PageId, TreeError> {
    let image = load_page(pages, parent)?;
    let fields = expect_internal(&image, parent)?;
    let items = read_internal_items(&image, &fields)?;

    if child == fields.left_child {
        if !right || items.is_empty() {
            return Ok(PageId::new(0));
        }

        return Ok(items[0].child);
    }

    for (index, item) in items.iter().enumerate() {
        if item.child != child {
            continue;
        }

        if right {
            return Ok(items
                .get(index + 1)
                .map(|next| next.child)
                .unwrap_or(PageId::new(0)));
        }

        if index == 0 {
            return Ok(fields.left_child);
        }

        return Ok(items[index - 1].child);
    }
    Err(TreeError::Corrupt)
}

fn set_parent<P: PageWrite>(pages: &mut P, id: PageId, parent: PageId) -> Result<(), TreeError> {
    update_u64(pages, id, PARENT_OFF, parent.as_u64())
}

fn set_sibling<P: PageWrite>(
    pages: &mut P,
    id: PageId,
    offset: usize,
    sibling: PageId,
) -> Result<(), TreeError> {
    let image = load_page(pages, id)?;
    expect_leaf(&image, id)?;
    update_u64(pages, id, offset, sibling.as_u64())
}

fn update_u64<P: PageWrite>(
    pages: &mut P,
    id: PageId,
    offset: usize,
    value: u64,
) -> Result<(), TreeError> {
    let mut image = load_page(pages, id)?;
    let header = checked_header(&image, id)?;
    write_u64(&mut image, offset, value)?;
    seal_image(&mut image, header.kind(), header.page_id(), header.lsn())?;
    store_page(pages, id, &image)
}

fn write_overflow<P: PageWrite>(pages: &mut P, value: &[u8]) -> Result<PageId, TreeError> {
    if value.is_empty() || value.len() > MAX_VALUE_LEN {
        return Err(TreeError::Corrupt);
    }

    let mut ids = Vec::new();
    let mut offset = 0usize;
    while offset < value.len() {
        ids.push(pages.allocate(PageKind::Overflow)?);

        let step = (value.len() - offset).min(OVERFLOW_CHUNK);
        offset = offset.checked_add(step).ok_or(TreeError::Corrupt)?;
    }

    if ids.len() > MAX_OVERFLOW_HOPS as usize {
        return Err(TreeError::Corrupt);
    }

    offset = 0;
    for (index, id) in ids.iter().copied().enumerate() {
        let step = (value.len() - offset).min(OVERFLOW_CHUNK);
        let next = ids.get(index + 1).copied().unwrap_or(PageId::new(0));
        write_overflow_page(pages, id, next, &value[offset..offset + step])?;
        offset += step;
    }

    ids.first().copied().ok_or(TreeError::Corrupt)
}

fn write_overflow_page<P: PageWrite>(
    pages: &mut P,
    id: PageId,
    next: PageId,
    chunk: &[u8],
) -> Result<(), TreeError> {
    if chunk.is_empty() || chunk.len() > OVERFLOW_CHUNK {
        return Err(TreeError::Corrupt);
    }

    let existing = load_page(pages, id)?;
    let header = checked_header(&existing, id)?;
    if header.kind() != PageKind::Overflow {
        return Err(TreeError::Corrupt);
    }

    let mut image = [0u8; PAGE_SIZE];
    write_u64(&mut image, OV_NEXT_OFF, next.as_u64())?;
    write_u32(
        &mut image,
        OV_LEN_OFF,
        u32::try_from(chunk.len()).map_err(|_| TreeError::Corrupt)?,
    )?;
    write_u32(&mut image, OV_RESERVED_OFF, 0)?;

    image
        .get_mut(OV_DATA_OFF..OV_DATA_OFF + chunk.len())
        .ok_or(TreeError::Corrupt)?
        .copy_from_slice(chunk);

    seal_image(&mut image, PageKind::Overflow, id, header.lsn())?;
    store_page(pages, id, &image)
}

fn read_overflow_into<P: PageRead>(
    pages: &P,
    head: PageId,
    logical_len: u64,
    out: &mut Vec<u8>,
) -> Result<(), TreeError> {
    if logical_len == 0 || logical_len > MAX_VALUE_LEN as u64 || is_zero(head) {
        return Err(TreeError::Corrupt);
    }

    let len = usize::try_from(logical_len).map_err(|_| TreeError::Corrupt)?;
    out.clear();
    out.reserve(len);

    let mut current = head;
    let mut hops = 0u32;

    while out.len() < len {
        hops = hops.checked_add(1).ok_or(TreeError::Corrupt)?;
        if hops > MAX_OVERFLOW_HOPS || is_zero(current) {
            return Err(TreeError::Corrupt);
        }

        let image = load_page(pages, current)?;
        let header = checked_header(&image, current)?;
        if header.kind() != PageKind::Overflow || read_u32(&image, OV_RESERVED_OFF)? != 0 {
            return Err(TreeError::Corrupt);
        }

        let chunk_len = read_u32(&image, OV_LEN_OFF)? as usize;
        let next = PageId::new(read_u64(&image, OV_NEXT_OFF)?);
        if chunk_len == 0 || chunk_len > OVERFLOW_CHUNK {
            return Err(TreeError::Corrupt);
        }

        let full = !is_zero(next);
        if full && chunk_len != OVERFLOW_CHUNK {
            return Err(TreeError::Corrupt);
        }

        let end = out.len().checked_add(chunk_len).ok_or(TreeError::Corrupt)?;
        if end > len {
            return Err(TreeError::Corrupt);
        }

        let data = image
            .get(OV_DATA_OFF..OV_DATA_OFF + chunk_len)
            .ok_or(TreeError::Corrupt)?;

        out.extend_from_slice(data);
        current = next;
    }

    if out.len() != len || !is_zero(current) {
        return Err(TreeError::Corrupt);
    }

    Ok(())
}

fn retire_overflow<P: PageWrite>(
    pages: &mut P,
    head: PageId,
    logical_len: u64,
) -> Result<(), TreeError> {
    if logical_len == 0 || logical_len > MAX_VALUE_LEN as u64 || is_zero(head) {
        return Err(TreeError::Corrupt);
    }

    let len = usize::try_from(logical_len).map_err(|_| TreeError::Corrupt)?;
    let mut seen = 0usize;
    let mut current = head;
    let mut hops = 0u32;

    while seen < len {
        hops = hops.checked_add(1).ok_or(TreeError::Corrupt)?;
        if hops > MAX_OVERFLOW_HOPS || is_zero(current) {
            return Err(TreeError::Corrupt);
        }

        let image = load_page(pages, current)?;
        let header = checked_header(&image, current)?;
        if header.kind() != PageKind::Overflow {
            return Err(TreeError::Corrupt);
        }

        let chunk_len = read_u32(&image, OV_LEN_OFF)? as usize;
        let next = PageId::new(read_u64(&image, OV_NEXT_OFF)?);
        if chunk_len == 0 || chunk_len > OVERFLOW_CHUNK {
            return Err(TreeError::Corrupt);
        }

        seen = seen.checked_add(chunk_len).ok_or(TreeError::Corrupt)?;
        if seen > len {
            return Err(TreeError::Corrupt);
        }

        let retiring = current;
        current = next;
        pages.retire(retiring)?;
    }

    if seen != len || !is_zero(current) {
        return Err(TreeError::Corrupt);
    }

    Ok(())
}

fn fill_acceptable<P: PageRead>(
    pages: &P,
    id: PageId,
    fields: &Fields,
    is_root: bool,
) -> Result<(), TreeError> {
    if is_root || (fields.payload as usize) >= MIN_PAYLOAD {
        return Ok(());
    }

    let siblings = match fields.kind {
        PageKind::BTreeLeaf => {
            let mut ids = Vec::new();
            if !is_zero(fields.left) {
                ids.push(fields.left);
            }
            if !is_zero(fields.right) {
                ids.push(fields.right);
            }
            ids
        }
        PageKind::BTreeInternal => {
            let mut ids = Vec::new();
            let left = sibling_child(pages, fields.parent, id, false)?;
            let right = sibling_child(pages, fields.parent, id, true)?;
            if !is_zero(left) {
                ids.push(left);
            }
            if !is_zero(right) {
                ids.push(right);
            }
            ids
        }
        _ => return Err(TreeError::Corrupt),
    };

    if siblings.is_empty() {
        return Err(TreeError::Corrupt);
    }

    for sibling in siblings {
        if pair_can_restore(pages, id, sibling)? {
            return Err(TreeError::Corrupt);
        }
    }
    Ok(())
}

fn pair_can_restore<P: PageRead>(
    pages: &P,
    id: PageId,
    sibling: PageId,
) -> Result<bool, TreeError> {
    let left_id;
    let right_id;
    let id_fields = load_fields(pages, id)?;

    if id_fields.kind == PageKind::BTreeLeaf {
        if id_fields.right == sibling {
            left_id = id;
            right_id = sibling;
        } else if id_fields.left == sibling {
            left_id = sibling;
            right_id = id;
        } else {
            return Err(TreeError::Corrupt);
        }

        let left_image = load_page(pages, left_id)?;
        let right_image = load_page(pages, right_id)?;
        let left_fields = expect_leaf(&left_image, left_id)?;
        let right_fields = expect_leaf(&right_image, right_id)?;
        let mut combined = read_leaf_items(&left_image, &left_fields)?;
        combined.extend(read_leaf_items(&right_image, &right_fields)?);

        return leaf_combined_can_restore(&combined);
    }

    let parent = id_fields.parent;
    let image = load_page(pages, parent)?;
    let parent_fields = expect_internal(&image, parent)?;
    let items = read_internal_items(&image, &parent_fields)?;
    let sibling_on_right = sibling_child(pages, parent, id, true)? == sibling;

    let (left_child, right_child) = if sibling_on_right {
        (id, sibling)
    } else {
        (sibling, id)
    };

    let Some(separator) = items.iter().find(|item| item.child == right_child) else {
        return Err(TreeError::Corrupt);
    };

    let left_image = load_page(pages, left_child)?;
    let right_image = load_page(pages, right_child)?;
    let left_node = expect_internal(&left_image, left_child)?;
    let right_node = expect_internal(&right_image, right_child)?;

    let mut combined = read_internal_items(&left_image, &left_node)?;
    combined.push(InternalItem {
        key: separator.key.clone(),
        child: right_node.left_child,
    });
    combined.extend(read_internal_items(&right_image, &right_node)?);

    internal_combined_can_restore(&combined)
}

fn leaf_combined_can_restore(combined: &[LeafItem]) -> Result<bool, TreeError> {
    if pack_leaf(combined)? <= BODY_CAP {
        return Ok(true);
    }

    let point = split_leaf_point(combined)?;
    let left = pack_leaf(&combined[..point])?;
    let right = pack_leaf(&combined[point..])?;

    Ok(left >= MIN_PAYLOAD && right >= MIN_PAYLOAD)
}

fn internal_combined_can_restore(combined: &[InternalItem]) -> Result<bool, TreeError> {
    if pack_internal(combined)? <= BODY_CAP {
        return Ok(true);
    }

    let point = split_internal_point(combined)?;
    let left = pack_internal(&combined[..point])?;
    let right = pack_internal(&combined[point + 1..])?;

    Ok(left >= MIN_PAYLOAD && right >= MIN_PAYLOAD)
}

fn verify_node<P: PageRead>(
    pages: &P,
    id: PageId,
    low: Option<&[u8]>,
    high: Option<&[u8]>,
    is_root: bool,
    depth: u32,
) -> Result<(), TreeError> {
    if depth > MAX_TREE_HOPS || is_zero(id) {
        return Err(TreeError::Corrupt);
    }

    let image = load_page(pages, id)?;
    let fields = read_fields(&image)?;
    fill_acceptable(pages, id, &fields, is_root)?;

    match fields.kind {
        PageKind::BTreeLeaf => {
            expect_leaf(&image, id)?;
            let items = read_leaf_items(&image, &fields)?;
            if items.is_empty() {
                return Err(TreeError::Corrupt);
            }

            for item in &items {
                if let Some(bound) = low
                    && cmp_bytes(&item.key, bound) == Ordering::Less
                {
                    return Err(TreeError::Corrupt);
                }

                if let Some(bound) = high
                    && cmp_bytes(&item.key, bound) != Ordering::Less
                {
                    return Err(TreeError::Corrupt);
                }

                if let StoredValue::Overflow { head, len } = &item.value {
                    let mut buf = Vec::new();
                    read_overflow_into(pages, *head, *len, &mut buf)?;
                }
            }
            Ok(())
        }
        PageKind::BTreeInternal => {
            expect_internal(&image, id)?;
            let items = read_internal_items(&image, &fields)?;
            if is_zero(fields.left_child) {
                return Err(TreeError::Corrupt);
            }

            expect_parent(pages, fields.left_child, id)?;
            let first_high = items.first().map(|item| item.key.as_slice()).or(high);
            verify_node(pages, fields.left_child, low, first_high, false, depth + 1)?;

            for (index, item) in items.iter().enumerate() {
                let min = subtree_min(pages, item.child)?;
                if min.as_slice() != item.key.as_slice() {
                    return Err(TreeError::Corrupt);
                }

                expect_parent(pages, item.child, id)?;
                let upper = items
                    .get(index + 1)
                    .map(|next| next.key.as_slice())
                    .or(high);
                verify_node(pages, item.child, Some(&item.key), upper, false, depth + 1)?;
            }
            Ok(())
        }
        _ => Err(TreeError::Corrupt),
    }
}

fn expect_parent<P: PageRead>(pages: &P, id: PageId, parent: PageId) -> Result<(), TreeError> {
    let fields = load_fields(pages, id)?;
    if fields.parent != parent {
        Err(TreeError::Corrupt)
    } else {
        Ok(())
    }
}

fn collect_leaves<P: PageRead>(
    pages: &P,
    id: PageId,
    out: &mut Vec<PageId>,
    depth: u32,
) -> Result<(), TreeError> {
    if depth > MAX_TREE_HOPS || is_zero(id) {
        return Err(TreeError::Corrupt);
    }

    let image = load_page(pages, id)?;
    let fields = read_fields(&image)?;

    match fields.kind {
        PageKind::BTreeLeaf => {
            out.push(id);
            Ok(())
        }
        PageKind::BTreeInternal => {
            collect_leaves(pages, fields.left_child, out, depth + 1)?;

            let items = read_internal_items(&image, &fields)?;
            for item in items {
                collect_leaves(pages, item.child, out, depth + 1)?;
            }

            Ok(())
        }
        _ => Err(TreeError::Corrupt),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{
        BODY_CAP, BODY_OFF, BTree, Cursor, FLAG_OVERFLOW, LEFT_OFF, MAX_KEY_LEN, MAX_VALUE_LEN,
        MIN_PAYLOAD, OV_NEXT_OFF, OVERFLOW_CHUNK, PageRead, PageWrite, TreeError, expect_internal,
        expect_leaf, load_page, read_internal_items, read_leaf_items, read_u16, write_u64,
    };
    use crate::page::{Lsn, PAGE_SIZE, PageHeader, PageId, PageKind};
    use crate::space::{INITIAL_PAGES, Space, SpaceError};

    struct MemPages {
        space: Space,
        images: HashMap<PageId, [u8; PAGE_SIZE]>,
        allocations: u64,
    }

    impl MemPages {
        fn new() -> Self {
            Self {
                space: Space::create(),
                images: HashMap::new(),
                allocations: 0,
            }
        }

        fn allocations(&self) -> u64 {
            self.allocations
        }

        fn high_water(&self) -> u64 {
            self.space.high_water()
        }

        fn is_retired(&self, id: PageId) -> bool {
            self.space.is_retired(id)
        }

        fn image_mut(&mut self, id: PageId) -> &mut [u8; PAGE_SIZE] {
            self.images.get_mut(&id).expect("page")
        }

        fn header(&self, id: PageId) -> PageHeader {
            PageHeader::decode(self.images.get(&id).expect("page")).expect("header")
        }

        fn contains_kind(&self, kind: PageKind) -> bool {
            self.images.values().any(|image| {
                PageHeader::decode(image)
                    .map(|header| header.kind() == kind)
                    .unwrap_or(false)
            })
        }
    }

    impl PageRead for MemPages {
        type Pin<'a> = &'a [u8];

        fn pin(&self, id: PageId) -> Result<Self::Pin<'_>, TreeError> {
            self.images
                .get(&id)
                .map(|image| image.as_slice())
                .ok_or(TreeError::Corrupt)
        }
    }

    impl PageWrite for MemPages {
        type PinMut<'a> = &'a mut [u8];

        fn allocate(&mut self, kind: PageKind) -> Result<PageId, TreeError> {
            let id = self.space.allocate(kind).map_err(map_space)?;
            let mut image = [0u8; PAGE_SIZE];
            PageHeader::new(kind, id, Lsn::new(0))
                .encode(&mut image)
                .expect("blank page");
            self.images.insert(id, image);
            self.allocations += 1;
            Ok(id)
        }

        fn retire(&mut self, id: PageId) -> Result<(), TreeError> {
            self.space.retire(id, Lsn::new(1)).map_err(map_space)
        }

        fn pin_mut(&mut self, id: PageId) -> Result<Self::PinMut<'_>, TreeError> {
            self.images
                .get_mut(&id)
                .map(|image| image.as_mut_slice())
                .ok_or(TreeError::Corrupt)
        }
    }

    fn map_space(error: SpaceError) -> TreeError {
        match error {
            SpaceError::Full => TreeError::Full,
            SpaceError::DoubleFree => TreeError::DoubleFree,
            SpaceError::Corrupt => TreeError::Corrupt,
        }
    }

    fn pairs(tree: &BTree, pages: &MemPages) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut cursor = tree.scan_from(pages, None).unwrap();
        let mut out = Vec::new();
        while let Some(entry) = cursor.next(pages).unwrap() {
            out.push((entry.key.to_vec(), entry.value.to_vec()));
        }
        out
    }

    fn map_pairs(map: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
        map.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn reseal(image: &mut [u8; PAGE_SIZE], header: PageHeader) {
        PageHeader::new(header.kind(), header.page_id(), header.lsn())
            .encode(image)
            .unwrap();
    }

    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, max: usize) -> usize {
            (self.next_u64() as usize) % max
        }
    }

    fn ordered_i64(value: i64) -> [u8; 8] {
        ((value as u64) ^ (1_u64 << 63)).to_be_bytes()
    }

    #[test]
    fn example_new_tree_is_empty() {
        assert_eq!(BTree::new().root(), PageId::new(0));
        assert_eq!(BTree::from_root(PageId::new(4)).root(), PageId::new(4));
        assert_eq!(MAX_KEY_LEN, 256);
        assert_eq!(
            MAX_VALUE_LEN,
            super::MAX_OVERFLOW_HOPS as usize * OVERFLOW_CHUNK
        );
        assert_eq!(TreeError::KeyTooLarge, TreeError::KeyTooLarge);
        assert_ne!(TreeError::Corrupt, TreeError::ValueTooLarge);
    }

    #[test]
    fn example_insert_get_remove() {
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"a", b"1").unwrap();
        assert_eq!(tree.get(&pages, b"a").unwrap().as_deref(), Some(&b"1"[..]));
        tree.insert(&mut pages, b"a", b"2").unwrap();
        assert_eq!(tree.get(&pages, b"a").unwrap().as_deref(), Some(&b"2"[..]));
        tree.remove(&mut pages, b"missing").unwrap();
        tree.remove(&mut pages, b"a").unwrap();
        assert_eq!(tree.get(&pages, b"a").unwrap(), None);
        assert_eq!(tree.root(), PageId::new(0));
    }

    #[test]
    fn example_scan_entries_borrow_the_cursor() {
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"k", b"v").unwrap();
        tree.insert(&mut pages, b"a", b"1").unwrap();
        tree.insert(&mut pages, b"b", b"2").unwrap();
        let mut cursor = tree.scan_from(&pages, Some(b"b")).unwrap();
        let entry = cursor.next(&pages).unwrap().unwrap();
        assert_eq!(entry.key, b"b");
        assert_eq!(entry.value, b"2");
        let owned = entry.key.to_vec();
        assert!(cursor.next(&pages).unwrap().is_some() || owned == b"b");
    }

    #[test]
    fn empty_scan_and_past_end_do_not_allocate() {
        let pages = MemPages::new();
        let tree = BTree::new();
        let before_water = pages.high_water();
        let before_alloc = pages.allocations();
        let mut cursor = tree.scan_from(&pages, None).unwrap();
        assert!(cursor.next(&pages).unwrap().is_none());
        assert_eq!(pages.high_water(), before_water);
        assert_eq!(pages.allocations(), before_alloc);
        assert_eq!(before_water, INITIAL_PAGES);

        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"only", b"v").unwrap();
        let mut cursor = tree.scan_from(&pages, None).unwrap();
        assert!(cursor.next(&pages).unwrap().is_some());
        let water = pages.high_water();
        let allocs = pages.allocations();
        assert!(cursor.next(&pages).unwrap().is_none());
        assert_eq!(pages.high_water(), water);
        assert_eq!(pages.allocations(), allocs);
        let mut finished = Cursor::empty();
        std::mem::swap(&mut finished, &mut cursor);
        assert!(finished.next(&pages).unwrap().is_none());
        assert_eq!(pages.allocations(), allocs);
    }

    #[test]
    fn prefix_and_empty_keys_sort_in_byte_order() {
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        let keys: &[&[u8]] = &[b"b", b"ab", b"a", b"a\0", b""];
        for key in keys {
            tree.insert(&mut pages, key, key).unwrap();
        }
        let scanned = pairs(&tree, &pages);
        let keys_only: Vec<&[u8]> = scanned.iter().map(|(key, _)| key.as_slice()).collect();
        assert_eq!(keys_only, vec![b"".as_slice(), b"a", b"a\0", b"ab", b"b"]);
        tree.verify(&pages).unwrap();
    }

    #[test]
    fn little_endian_i64_sorts_as_unsigned_bytes() {
        let values = [-2_i64, -1, 0, 1];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        for value in values {
            tree.insert(&mut pages, &value.to_le_bytes(), &value.to_le_bytes())
                .unwrap();
        }
        let scanned: Vec<[u8; 8]> = pairs(&tree, &pages)
            .into_iter()
            .map(|(key, _)| key.try_into().unwrap())
            .collect();
        let mut expected: Vec<[u8; 8]> = values.iter().map(|value| value.to_le_bytes()).collect();
        expected.sort();
        assert_eq!(scanned, expected);
        let numeric: Vec<i64> = scanned.iter().map(|key| i64::from_le_bytes(*key)).collect();
        assert_ne!(numeric, values);
        tree.verify(&pages).unwrap();
    }

    #[test]
    fn ordered_i64_bytes_sort_numerically() {
        let values = [2_i64, -2, 0, -1, 1];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        for value in values {
            tree.insert(&mut pages, &ordered_i64(value), &value.to_be_bytes())
                .unwrap();
        }
        let scanned: Vec<i64> = pairs(&tree, &pages)
            .into_iter()
            .map(|(_, value)| i64::from_be_bytes(value.try_into().unwrap()))
            .collect();
        assert_eq!(scanned, vec![-2, -1, 0, 1, 2]);
        tree.verify(&pages).unwrap();
    }

    #[test]
    fn leaf_split_separator_is_right_minimum() {
        let value_len = MIN_PAYLOAD - 8 - 1;
        let value = vec![0x5A; value_len];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        for key in [b"a".as_slice(), b"m", b"t"] {
            tree.insert(&mut pages, key, &value).unwrap();
        }
        tree.verify(&pages).unwrap();
        let root = tree.root();
        let root_image = load_page(&pages, root).unwrap();
        let root_fields = expect_internal(&root_image, root).unwrap();
        let separators = read_internal_items(&root_image, &root_fields).unwrap();
        assert_eq!(separators.len(), 1);
        assert_eq!(separators[0].key, b"m");

        let left_image = load_page(&pages, root_fields.left_child).unwrap();
        let left_fields = expect_leaf(&left_image, root_fields.left_child).unwrap();
        let left = read_leaf_items(&left_image, &left_fields).unwrap();
        let right_image = load_page(&pages, separators[0].child).unwrap();
        let right_fields = expect_leaf(&right_image, separators[0].child).unwrap();
        let right = read_leaf_items(&right_image, &right_fields).unwrap();
        assert!(
            left.iter()
                .all(|item| item.key.as_slice() < b"m".as_slice())
        );
        assert!(
            right
                .iter()
                .all(|item| item.key.as_slice() >= b"m".as_slice())
        );
        assert_eq!(right[0].key, b"m");
        assert_eq!(left_fields.right, separators[0].child);
        assert_eq!(right_fields.left, root_fields.left_child);
    }

    #[test]
    fn root_collapses_after_delete() {
        let value = vec![0x11; MIN_PAYLOAD - 8 - 1];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        let mut map = BTreeMap::new();
        for key in [b"a".as_slice(), b"m", b"t", b"z"] {
            tree.insert(&mut pages, key, &value).unwrap();
            map.insert(key.to_vec(), value.clone());
            assert_eq!(pairs(&tree, &pages), map_pairs(&map));
            tree.verify(&pages).unwrap();
        }
        assert_ne!(tree.root(), PageId::new(0));
        assert!(pages.contains_kind(PageKind::BTreeInternal));
        for key in [b"a".as_slice(), b"m", b"t", b"z"] {
            tree.remove(&mut pages, key).unwrap();
            map.remove(key);
            assert_eq!(pairs(&tree, &pages), map_pairs(&map));
            tree.verify(&pages).unwrap();
        }
        assert_eq!(tree.root(), PageId::new(0));
        assert!(pairs(&tree, &pages).is_empty());
    }

    #[test]
    fn overflow_round_trip_update_and_retire() {
        let big = vec![0xAB; OVERFLOW_CHUNK + 32];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"blob", &big).unwrap();
        assert_eq!(
            tree.get(&pages, b"blob").unwrap().as_deref(),
            Some(big.as_slice())
        );
        assert!(pages.contains_kind(PageKind::Overflow));
        let root = tree.root();
        let image = load_page(&pages, root).unwrap();
        let fields = expect_leaf(&image, root).unwrap();
        let items = read_leaf_items(&image, &fields).unwrap();
        let super::StoredValue::Overflow { head, len } = items[0].value else {
            panic!("expected overflow");
        };
        assert_eq!(len, big.len() as u64);
        tree.insert(&mut pages, b"blob", b"small").unwrap();
        assert_eq!(
            tree.get(&pages, b"blob").unwrap().as_deref(),
            Some(&b"small"[..])
        );
        assert!(pages.is_retired(head));
        tree.remove(&mut pages, b"blob").unwrap();
        assert_eq!(tree.get(&pages, b"blob").unwrap(), None);
        tree.verify(&pages).unwrap();
    }

    #[test]
    fn key_and_value_limits_allocate_nothing() {
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        let allocs = pages.allocations();
        let water = pages.high_water();
        let long_key = vec![0_u8; MAX_KEY_LEN + 1];
        assert_eq!(
            tree.insert(&mut pages, &long_key, b"v").unwrap_err(),
            TreeError::KeyTooLarge
        );
        let long_value = vec![0_u8; MAX_VALUE_LEN + 1];
        assert_eq!(
            tree.insert(&mut pages, b"k", &long_value).unwrap_err(),
            TreeError::ValueTooLarge
        );
        assert_eq!(pages.allocations(), allocs);
        assert_eq!(pages.high_water(), water);
        assert_eq!(tree.root(), PageId::new(0));
    }

    #[test]
    fn corrupt_sibling_order_and_overflow() {
        let value = vec![0x22; MIN_PAYLOAD - 8 - 1];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        for key in [b"a".as_slice(), b"m", b"t"] {
            tree.insert(&mut pages, key, &value).unwrap();
        }
        let root = tree.root();
        let root_image = load_page(&pages, root).unwrap();
        let root_fields = expect_internal(&root_image, root).unwrap();
        let right = read_internal_items(&root_image, &root_fields).unwrap()[0].child;
        let allocs = pages.allocations();
        let header = pages.header(right);
        {
            let image = pages.image_mut(right);
            write_u64(image, LEFT_OFF, 0).unwrap();
            reseal(image, header);
        }
        assert_scan_corrupt(&tree, &pages);
        assert_eq!(pages.allocations(), allocs);
        let left = root_fields.left_child.as_u64();
        let header = pages.header(right);
        {
            let image = pages.image_mut(right);
            write_u64(image, LEFT_OFF, left).unwrap();
            reseal(image, header);
        }

        let right_image = load_page(&pages, right).unwrap();
        let right_fields = expect_leaf(&right_image, right).unwrap();
        let items = read_leaf_items(&right_image, &right_fields).unwrap();
        assert!(items.len() >= 2);
        let first = BODY_OFF + 4;
        let second = {
            let key_len = read_u16(&right_image, BODY_OFF).unwrap() as usize;
            let inline = 4 + key_len + 4 + value.len();
            BODY_OFF + inline + 4
        };
        let header = pages.header(right);
        {
            let image = pages.image_mut(right);
            image.swap(first, second);
            reseal(image, header);
        }
        assert_scan_corrupt(&tree, &pages);
        assert_eq!(pages.allocations(), allocs);

        let big = vec![0xCD; OVERFLOW_CHUNK + 8];
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"blob", &big).unwrap();
        let image = load_page(&pages, tree.root()).unwrap();
        let fields = expect_leaf(&image, tree.root()).unwrap();
        let super::StoredValue::Overflow { head, .. } =
            read_leaf_items(&image, &fields).unwrap()[0].value
        else {
            panic!("overflow");
        };
        let allocs = pages.allocations();
        let header = pages.header(head);
        {
            let image = pages.image_mut(head);
            write_u64(image, OV_NEXT_OFF, head.as_u64()).unwrap();
            reseal(image, header);
        }
        assert_eq!(tree.get(&pages, b"blob").unwrap_err(), TreeError::Corrupt);
        assert_eq!(pages.allocations(), allocs);

        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"blob", &big).unwrap();
        let allocs = pages.allocations();
        let root = tree.root();
        let header = pages.header(root);
        {
            let image = pages.image_mut(root);
            let key_len = read_u16(image, BODY_OFF).unwrap() as usize;
            assert_eq!(image[BODY_OFF + 2], FLAG_OVERFLOW);
            let len_at = BODY_OFF + 4 + key_len + 8;
            write_u64(image, len_at, u64::MAX).unwrap();
            reseal(image, header);
        }
        assert_eq!(tree.get(&pages, b"blob").unwrap_err(), TreeError::Corrupt);
        assert_scan_corrupt(&tree, &pages);
        assert_eq!(pages.allocations(), allocs);
    }

    fn assert_scan_corrupt(tree: &BTree, pages: &MemPages) {
        match tree.scan_from(pages, None) {
            Err(TreeError::Corrupt) => {}
            Ok(mut cursor) => loop {
                match cursor.next(pages) {
                    Err(TreeError::Corrupt) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("scan ended without Corrupt"),
                    Err(error) => panic!("unexpected {error:?}"),
                }
            },
            Err(error) => panic!("unexpected {error:?}"),
        }
    }

    #[test]
    fn scan_from_matches_map_range() {
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        let mut map = BTreeMap::new();
        for key in [b"".as_slice(), b"a", b"a\0", b"ab", b"b", b"m"] {
            tree.insert(&mut pages, key, key).unwrap();
            map.insert(key.to_vec(), key.to_vec());
        }
        let mut cursor = tree.scan_from(&pages, Some(b"ab")).unwrap();
        let mut scanned = Vec::new();
        while let Some(entry) = cursor.next(&pages).unwrap() {
            scanned.push(entry.key.to_vec());
        }
        let expected: Vec<Vec<u8>> = map
            .range(b"ab".to_vec()..)
            .map(|(key, _)| key.clone())
            .collect();
        assert_eq!(scanned, expected);
    }

    #[test]
    fn random_history_matches_btreemap() {
        let mut rng = Rng(0x5A17_BEEF_1234_5678);
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let mut next_key = 0u32;
        let mut saw_internal = false;
        let mut saw_overflow = false;

        for step in 0..700 {
            let delete = rng.below(10) < 2 && !map.is_empty();
            if delete {
                let index = rng.below(map.len());
                let key = map.keys().nth(index).expect("key").clone();
                tree.remove(&mut pages, &key).unwrap();
                map.remove(&key);
            } else {
                let key = if rng.below(8) == 0 {
                    history_prefix(rng.below(5))
                } else {
                    let key = next_key.to_be_bytes().to_vec();
                    next_key += 1;
                    key
                };
                let value = history_value(&mut rng, step);
                if value.len() > BODY_CAP {
                    saw_overflow = true;
                }
                tree.insert(&mut pages, &key, &value).unwrap();
                map.insert(key, value);
            }
            if pages.contains_kind(PageKind::BTreeInternal) {
                saw_internal = true;
            }
            assert_eq!(pairs(&tree, &pages), map_pairs(&map), "step {step}");
            tree.verify(&pages).unwrap();
        }

        assert!(saw_internal);
        assert!(saw_overflow);
        let keys: Vec<Vec<u8>> = map.keys().cloned().collect();
        for key in keys {
            tree.remove(&mut pages, &key).unwrap();
            map.remove(&key);
            assert_eq!(pairs(&tree, &pages), map_pairs(&map));
            tree.verify(&pages).unwrap();
        }
        assert_eq!(tree.root(), PageId::new(0));
        assert!(pairs(&tree, &pages).is_empty());
    }

    fn history_prefix(kind: usize) -> Vec<u8> {
        match kind {
            0 => Vec::new(),
            1 => b"a".to_vec(),
            2 => b"a\0".to_vec(),
            3 => b"ab".to_vec(),
            _ => b"b".to_vec(),
        }
    }

    fn history_value(rng: &mut Rng, step: u32) -> Vec<u8> {
        if step.is_multiple_of(47) {
            return vec![0x3C; OVERFLOW_CHUNK + (step as usize % 40) + 1];
        }
        let len = rng.below(24);
        (0..len).map(|_| rng.next_u64() as u8).collect()
    }

    #[test]
    fn same_value_does_not_allocate_again() {
        let mut pages = MemPages::new();
        let mut tree = BTree::new();
        tree.insert(&mut pages, b"a", b"same").unwrap();
        let allocs = pages.allocations();
        tree.insert(&mut pages, b"a", b"same").unwrap();
        assert_eq!(pages.allocations(), allocs);
    }
}
