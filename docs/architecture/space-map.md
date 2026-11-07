# Space map

Format 1.0. The superblock minor stays 0. The bitmap chain is part of that layout.

The allocator header is one `Allocator` page. The first bitmap page is [`BITMAP_PAGE_ID`](../../crates/pizzlydb-storage/src/page.rs). Each bitmap page stores allocation bits in the body and a little-endian page id in the last 8 bytes. That id is the next bitmap page, or zero at the end of the chain.

One bitmap page covers 65,248 page ids. The next page's id is the first id of the range it covers, and that id's bit is set so the bitmap page is not handed out as a data page. `MAX_ADDRESSABLE_PAGES` is 2^32.

In memory each bitmap page is an `Arc` of its payload. Cloning an allocator clones those pointers. A bit update copies only the chunk it changes.
