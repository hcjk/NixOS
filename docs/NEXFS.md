# NexFS v1 on-disk format

NexFS is NexOS's native little-endian filesystem. Version 1 deliberately uses
fixed-size metadata, one allocation bitmap per resource type, and synchronous
metadata updates. It does not claim compatibility with ext, UFS, FAT, or any
Linux filesystem.

## Compatibility rules

- Superblock magic: `NEXFS\x01\0\0`
- Format version: `1`
- Logical filesystem block size: 4096 bytes
- Supported device sector sizes must divide 4096 bytes.
- Inodes and directory entries are 256 bytes.
- All integer fields are little-endian.
- Superblocks, allocated inodes, and directory entries carry CRC-32 checksums.
- Unknown format versions or invalid layout relationships are rejected.

The superblock lives in block 1. Block 0 is reserved for future boot or volume
metadata. Block 2 contains the inode bitmap. The block allocation bitmap starts
in block 3 and grows according to the volume size. The inode table follows the
block bitmap, and data blocks follow the inode table.

Inode 0 is reserved and inode 1 is the root directory. The inode table has a
minimum of four blocks and a maximum of 2,048 blocks, allowing up to 32,768
inodes. Unused inode-table records are zeroed during formatting.

## Inodes and file data

Each inode records:

- type (`regular` or `directory`);
- reserved permission bits;
- byte size;
- creation and modification timestamps;
- twelve direct data-block numbers;
- one single-indirect block containing 512 additional block numbers;
- allocated data-block count and metadata generation;
- CRC-32 checksum.

NexFS v1 files therefore contain at most 524 data blocks, or 2,146,304 bytes.
Writes may be sequential or random. Gaps created by writes beyond end-of-file
are zero-filled. Truncation releases blocks in reverse order and clears the
unused tail of a partially retained block.

Directory contents are compact arrays of 256-byte entries. Each entry contains
an inode number, type, UTF-8 name of at most 240 bytes, and CRC-32 checksum.
`.` and `..` are resolved by the filesystem layer and are not stored as
directory entries. Version 1 does not support hard links.

## Update ordering

NexFS v1 has no journal. A writable mount clears the clean flag and flushes the
superblock before any metadata can change. Allocation bits are persisted before
new blocks or inodes are linked into a directory. Removal unlinks the directory
entry before releasing the inode and its blocks. Each completed mutation
flushes its dirty superblock generation.

Unmount first flushes outstanding writes, then sets the clean flag, writes the
new generation, and flushes again. Dropping a mounted filesystem without
calling `unmount` intentionally leaves the volume dirty.

The offline checker refuses dirty volumes and validates:

- superblock and metadata checksums;
- metadata layout and device bounds;
- reserved and out-of-range bitmap bits;
- inode allocation and root-directory type;
- direct and indirect block bounds;
- missing, leaked, and multiply allocated blocks;
- directory checksums, names, duplicate names, and target types;
- directory reachability and orphaned inodes.

Future format versions may add extents, journaling, links, access-control
enforcement, and online crash recovery. Those changes must use a new compatible
feature flag or format version rather than silently changing v1 metadata.
