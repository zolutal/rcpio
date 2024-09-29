pub(crate) const NEWC_MAGIC: &[u8] = b"070701";
pub(crate) const CRC_MAGIC: &[u8]  = b"070702";

pub(crate) const TRAILER: &[u8] = b"00000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000B00000000TRAILER!!!\0";

pub(crate) const CPIO_MAGIC_LEN: usize = 6;
pub(crate) const CPIO_FIELD_LEN: usize = 8;

/// Total size of a NEWC/CRC cpio entry header
pub(crate) const CPIO_HEADER_LEN: usize = 110;

/// POSIX file mode constants
pub(crate) const S_IFMT   : u64 = 0o170000; // bit mask file type bit field
pub(crate) const S_IFSOCK : u64 = 0o140000; // socket
pub(crate) const S_IFLNK  : u64 = 0o120000; // symbolic link
pub(crate) const S_IFREG  : u64 = 0o100000; // regular file
pub(crate) const S_IFBLK  : u64 = 0o060000; // block device
pub(crate) const S_IFDIR  : u64 = 0o040000; // directory
pub(crate) const S_IFCHR  : u64 = 0o020000; // character device
pub(crate) const S_IFIFO  : u64 = 0o010000; // FIFO
pub(crate) const MODE_R: u64 = 0o04;
pub(crate) const MODE_W: u64 = 0o02;
pub(crate) const MODE_X: u64 = 0o01;
