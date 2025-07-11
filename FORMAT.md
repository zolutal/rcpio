# FORMAT DESCRIPTION

This document serves to detail the cpio archive format.

## cpio

A cpio file consists of many cpio 'entries', the contents of which are described below, concatenated together followed by a 'trailer' entry at the end  of the file.

## cpio Entries

cpio entries are always one of two types: NEWC or CRC, the two types of entries are nearly identical, though for the CRC entries a checksum is included.

- 0x00: magic      -  either '070701' (NEWC) or '070702' (CRC)
- 0x06: inode      -  as an 8 byte hex string, with no 0x prefix
- 0x0e: mode       -  as an 8 byte hex string, with no 0x prefix
- 0x16: uid        -  as an 8 byte hex string, with no 0x prefix
- 0x1e: gid        -  as an 8 byte hex string, with no 0x prefix
- 0x26: nlink      -  as an 8 byte hex string, with no 0x prefix
- 0x2e: mtime      -  as an 8 byte hex string, with no 0x prefix
- 0x36: filesize   -  as an 8 byte hex string, with no 0x prefix
- 0x3e: devmajor   -  as an 8 byte hex string, with no 0x prefix
- 0x46: devminor   -  as an 8 byte hex string, with no 0x prefix
- 0x4e: rdevmajor  -  as an 8 byte hex string, with no 0x prefix
- 0x56: rdevminor  -  as an 8 byte hex string, with no 0x prefix
- 0x5e: namesize   -  as an 8 byte hex string, with no 0x prefix
- 0x66: checksum
    - for regular files: zero for NEWC and crc of contents for CRC, in 8 byte hex string format, with no 0x prefix
    - for other files, zero in 8 bytes hex string format, regardless of NEWC/CRC mode (including symlinks which have a content of the target path)
- 0x6e: path       -  as a null-terminated string
- 0x??: padding    -  file content is padded to 4 byte alignment (from start of archive being written out)
- 0x??: content
    - for regular files, the contents of the file
    - for sylinks, the content is the target path of the symlink
    - for directories, the content is empty
- 0x??: padding    -  cpio entries are padded to 4 byte alignment (from start of archive being written out)

## Trailer

The cpio format ends with a "trailer", a sequence of characters denoting the end of the file:
"00000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000B00000000TRAILER"

Note: GNU cpio seems to pad the file with zeroes at the end of the file, following the trailer, to a 0x200 byte alignment.
