mod defs;
use defs::{CPIO_FIELD_LEN, CPIO_HEADER_LEN, CPIO_MAGIC_LEN};

use std::fs::{create_dir, read_link, symlink_metadata, File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::linux::fs::MetadataExt;
use std::os::unix::fs::{symlink, FileTypeExt, PermissionsExt};
use std::str::from_utf8;
use std::path::{Path, PathBuf};

use fallible_iterator::FallibleIterator;
use flate2::write::GzEncoder;
use flate2::Compression;

/// Error type for parsing cpio archives
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Failed to load archive into memory")]
    CpioLoadError,

    #[error("Unexpected end of file")]
    EarlyEOFError,

    #[error("Invalid archive format: {0}")]
    InvalidArchiveError(String),

    #[error("Cpio entry conversion error: {0}")]
    EntryConversionError(String),

    #[error("Invalid or unsupported posix file mode: {0}")]
    FileModeError(String),

    #[error("File system error: {0}")]
    FileSystemError(String),

    #[error("Gzip encoder error: {0}")]
    GzEncoderError(String),

    #[error("No such file in archive: {0}")]
    NoSuchFile(String),

    #[error("String encoding errror: {0}")]
    StringEncodingError(String),
}

#[derive(Debug, Clone, Copy)]
pub enum CpioFormat {
    Newc,
    Crc,
}

fn identify_format(mem: &[u8]) -> Result<CpioFormat, Error> {
    if mem.starts_with(defs::NEWC_MAGIC) {
        Ok(CpioFormat::Newc)
    } else if mem.starts_with(defs::CRC_MAGIC){
        Ok(CpioFormat::Crc)
    } else {
        Err(Error::InvalidArchiveError(String::from("Unrecognized Format")))
    }
}

/// Convert the file permissions portion of a file mode to a representative string
fn mode_perm_to_str(mode: u64, shift: usize) -> String {
    let mode = (mode >> shift) & 0o7;
    let mut perm_string = String::new();

    if mode & defs::MODE_R != 0 {
        perm_string.push('r');
    } else {
        perm_string.push('-');
    }

    if mode & defs::MODE_W != 0 {
        perm_string.push('w');
    } else {
        perm_string.push('-');
    }

    if mode & defs::MODE_X != 0 {
        perm_string.push('x');
    } else {
        perm_string.push('-');
    }

    perm_string
}

/// Convert the octal representation of a file mode to a representative string
fn mode_to_str(mode: u64) -> Result<String, Error> {
    let mut mode_str = String::new();

    if mode & defs::S_IFMT == 0 {
        return Err(Error::FileModeError(format!("{mode:o}")))
    }

    match mode & defs::S_IFMT {
        defs::S_IFSOCK => mode_str.push('s'),
        defs::S_IFLNK  => mode_str.push('l'),
        defs::S_IFREG  => mode_str.push('-'),
        defs::S_IFBLK  => mode_str.push('b'),
        defs::S_IFDIR  => mode_str.push('d'),
        defs::S_IFCHR  => mode_str.push('c'),
        defs::S_IFIFO  => mode_str.push('b'),
        _ => {
            return Err(Error::FileModeError(format!("{mode:o}")))
        }
    }

    mode_str.push_str(&mode_perm_to_str(mode, 6));
    mode_str.push_str(&mode_perm_to_str(mode, 3));
    mode_str.push_str(&mode_perm_to_str(mode, 0));

    Ok(mode_str)
}

struct CpioBuilderEntry {
    c_ino       : u32,
    c_mode      : u32,
    c_uid       : u32,
    c_gid       : u32,
    c_nlink     : u32,
    c_mtime     : u32,
    c_filesize  : u32,
    c_devmajor  : u32,
    c_devminor  : u32,
    c_rdevmajor : u32,
    c_rdevminor : u32,
    c_namesize  : u32,
    c_check     : u32,
}

impl CpioBuilderEntry {
    pub(crate) fn to_bytes(&self, format: &CpioFormat) -> Vec<u8> {
        let mut out = vec![];

        match format {
            CpioFormat::Newc => {
                out.append(&mut defs::NEWC_MAGIC.to_vec());
            },
            CpioFormat::Crc => {
                out.append(&mut defs::CRC_MAGIC.to_vec());
            },
        }

        let mut entry_str = String::new();

        entry_str.push_str(&format!("{:08x}", &self.c_ino));
        entry_str.push_str(&format!("{:08x}", &self.c_mode));
        entry_str.push_str(&format!("{:08x}", &self.c_uid));
        entry_str.push_str(&format!("{:08x}", &self.c_gid));
        entry_str.push_str(&format!("{:08x}", &self.c_nlink));
        entry_str.push_str(&format!("{:08x}", &self.c_mtime));
        entry_str.push_str(&format!("{:08x}", &self.c_filesize));
        entry_str.push_str(&format!("{:08x}", &self.c_devmajor));
        entry_str.push_str(&format!("{:08x}", &self.c_devminor));
        entry_str.push_str(&format!("{:08x}", &self.c_rdevmajor));
        entry_str.push_str(&format!("{:08x}", &self.c_rdevminor));
        entry_str.push_str(&format!("{:08x}", &self.c_namesize));
        entry_str.push_str(&format!("{:08x}", &self.c_check));

        out.append(&mut entry_str.to_uppercase().as_bytes().to_vec());
        out
    }
}

fn major(dev: u32) -> u32 {
    (dev >> 8) & 0xfff // major is bits 8–19
}

fn minor(dev: u32) -> u32 {
    (dev & 0xff) | ((dev >> 12) & 0xfff00) // minor is bits 0–7 and 20–31
}

pub struct CpioBuilder {
    format: CpioFormat,
    entries: Vec<(PathBuf, String)>
}

fn entry_bytes(
    fs_path: &Path,
    internal_path: &str,
    curr_len: usize,
    format: CpioFormat,
    inode_override: Option<u32>
) -> Result<Vec<u8>, Error> {
    let symlink_meta = symlink_metadata(fs_path).map_err(|e| {
        Error::FileSystemError(
            format!(
                "Failed to get metadata for symlink, {e}: {}",
                fs_path.to_string_lossy()
            )
        )
    })?;

    let mut content = vec![];
    let meta = if !symlink_meta.is_symlink() {
        let mut fp = File::open(fs_path).map_err(|_|
            Error::FileSystemError(
                format!("failed to read to end of file {}", fs_path.to_string_lossy())
            )
        )?;
        let meta = fp.metadata().map_err(|_| {
            Error::FileSystemError(
                format!("Failed to get metadata for file {}", fs_path.to_string_lossy())
            )
        })?;

        // nothing to read if path is "."
        if internal_path != "." && meta.is_file() {
            fp.read_to_end(&mut content).map_err(|_|
                Error::FileSystemError(
                    format!("failed to read to end of file {}", fs_path.to_string_lossy())
                )
            )?;
        }
        meta
    } else {
        // for symlinks the target path goes where the file content would
        let target_path = read_link(fs_path).map_err(|_| {
            Error::FileSystemError(
                format!("Failed to read symlink target for {}", fs_path.to_string_lossy())
            )
        })?;
        content.append(&mut target_path.to_string_lossy().to_string().as_bytes().to_vec());
        symlink_meta
    };

    let check: u32 = match format {
        CpioFormat::Newc => 0,
        CpioFormat::Crc => {
            // COMPAT: for symlinks the CRC value is zero
            if meta.is_symlink() {
                0
            } else {
                let mut res = 0u32;
                for b in &content {
                    res = res.wrapping_add(*b as u32);
                }
                res
            }
        }
    };

    let mut entry_data: Vec<u8> = vec![];

    let entry = CpioBuilderEntry {
        c_ino       : meta.st_ino() as u32,
        c_mode      : meta.st_mode(),
        c_uid       : meta.st_uid(),
        c_gid       : meta.st_gid(),
        c_nlink     : meta.st_nlink() as u32,
        c_mtime     : meta.st_mtime() as u32,
        c_filesize  : content.len() as u32,
        c_devmajor  : major(meta.st_dev() as u32),
        c_devminor  : minor(meta.st_dev() as u32),
        c_rdevmajor : major(meta.st_rdev() as u32),
        c_rdevminor : minor(meta.st_rdev() as u32),
        c_namesize  : (internal_path.len() + 1) as u32,
        c_check     : check,
    };

    entry_data.append(&mut entry.to_bytes(&format));

    // null-terminated internal path
    entry_data.append(&mut internal_path.as_bytes().to_vec());
    entry_data.push(0);

    // pad to four byte alignment before start of file contents
    let curr = curr_len + entry_data.len();
    if curr % 4 != 0 {
        entry_data.resize(entry_data.len() + (4 - (curr % 4)), 0)
    }

    entry_data.append(&mut content);

    // pad to four byte alignment at the end of file contents
    let curr = curr_len + entry_data.len();
    if curr % 4 != 0 {
        entry_data.resize(entry_data.len() + (4 - (curr % 4)), 0)
    }

    Ok(entry_data)
}

fn trailer_bytes(format: CpioFormat) -> Vec<u8> {
    let mut out = vec![];
    let magic = match format {
        CpioFormat::Newc => defs::NEWC_MAGIC,
        CpioFormat::Crc => defs::CRC_MAGIC,
    };

    out.append(&mut magic.to_vec());
    out.append(&mut defs::TRAILER.to_vec());

    out
}

impl CpioBuilder {
    pub fn new(format: CpioFormat) -> Self {
        CpioBuilder { format, entries: vec![] }
    }

    pub fn insert(
        &mut self, fs_path: &Path,
        archive_path: &str
    ) -> Result<(), Error>{
        let archive_path = if archive_path.is_empty() {
            "."
        } else {
            archive_path
        };

        self.entries.push((fs_path.to_path_buf(), archive_path.to_string()));

        Ok(())
    }

    pub fn write(&self, archive_path: &PathBuf, gzip: bool) -> Result<(), Error> {

        let mut out: Vec<u8> = vec![];

        let mut encoder = if gzip {
            let out_fp = File::create(archive_path).map_err(|_|
                Error::FileSystemError(
                    format!("Failed to create output file for gzip stream {}", archive_path.to_string_lossy())
                )
            )?;
            Some(GzEncoder::new(out_fp, Compression::default()))
        } else {
            None
        };

        for (fs_path, internal_path) in &self.entries {
            out.append(&mut entry_bytes(fs_path, internal_path, out.len(), self.format, None)?);
        }

        // write trailer
        out.append(&mut trailer_bytes(self.format));

        // pad to 0x100 alignment
        let mut padding = vec![];
        if out.len() % 200 != 0 {
            padding.resize(0x200 - (out.len() % 0x200), 0)
        }
        out.append(&mut padding);

        if let Some(ref mut encoder) = encoder {
            encoder.write_all(&out).map_err(|_|
                Error::GzEncoderError(String::from("failed when writing to encoder"))
            )?;
        }

        if gzip {
            if let Some(encoder) = encoder {
                encoder.finish().map_err(|_|
                    Error::GzEncoderError(String::from("failed when calling 'finish()' on encoder"))
                )?;
            }
        } else {
            let mut out_fp = File::create(archive_path).map_err(|_|
                Error::FileSystemError(
                    format!("Failed to create output file {}", archive_path.to_string_lossy())
                )
            )?;
            out_fp.write(&out).map_err(|_|
                Error::FileSystemError(String::from("failed to write data to archive file"))
            )?;
        }

        Ok(())
    }
}



pub struct Cpio<'a> {
    mem: &'a [u8],
    format: CpioFormat
}

impl<'a> Cpio<'a> {
    pub fn load(mem: &'a [u8]) -> Result<Self, Error> {
        let format = identify_format(mem)?;
        Ok(Cpio { mem, format })
    }

    pub fn iter_files(&self) -> CpioEntryIter<'a> {
        CpioEntryIter { index: 0, archive_mem: self.mem, format: self.format, trailer_seen: false }
    }

    pub fn extract_one(&self, output_path: &Path, entry: &CpioEntry) -> Result<(), Error> {
        let path = String::from_utf8(entry.name()?.to_vec()).map_err(|e|
            Error::StringEncodingError(e.to_string())
        )?;
        let trimmed_path = path.trim_end_matches('\0');


        let joined_path = std::path::absolute(output_path.join(trimmed_path)).map_err(|e| {
            Error::FileSystemError(e.to_string())
        })?;

        if joined_path == output_path {
            return Ok(())
        }

        if !joined_path.starts_with(output_path) {
            return Err(Error::FileSystemError("Encountered path was outside output directory".to_string()))
        }

        if entry.is_reg()? {
            let mut fp = OpenOptions::new().write(true).create_new(true).open(&joined_path).map_err(|e| {
                Error::FileSystemError(format!("{}: {}", e, joined_path.display()))
            })?;

            fp.write(entry.file_content()?).map_err(|e| {
                Error::FileSystemError(format!("{}: {}", e, joined_path.display()))
            })?;

            fp.set_permissions(Permissions::from_mode(entry.mode()? as u32)).map_err(|e| {
                Error::FileSystemError(format!("{}: {}", e, joined_path.display()))
            })?;
        } else if entry.is_dir()? {
            create_dir(&joined_path).map_err(|e| {
                Error::FileSystemError(format!("{}: {}", e, joined_path.display()))
            })?;
        } else if entry.is_link()? {
            let target_path = String::from_utf8(entry.file_content()?.to_vec()).map_err(|e|
                Error::FileSystemError(e.to_string())
            )?;
            let target_path = target_path.trim_end_matches('\0');
            symlink(target_path, joined_path).map_err(|e|
                Error::FileSystemError(e.to_string())
            )?;
        } else {
            unimplemented!("Entry type was none of: reg, dir, link")
        }

        Ok(())

    }

    pub fn push(&self, archive_path: &Path, fs_path: &Path, internal_path: &str) -> Result<(), Error> {
        let mut trailer_index: Option<usize> = None;
        let mut trailer_format: Option<CpioFormat> = None;

        let mut iter = self.iter_files();
        while let Some(entry) = &iter.next()? {
            let entry_path = String::from_utf8(entry.name()?.to_vec()).map_err(|e|
                Error::StringEncodingError(e.to_string())
            )?;

            if internal_path == entry_path.trim_end_matches(char::from(0)) {
                panic!("Cannot push a file to a path that already exists in the archive")
            }

            if entry.is_trailer()? {
                trailer_index = Some(entry.index);
                trailer_format = Some(entry.format);
            }
        }

        let trailer_index: usize = if let Some(trailer_index) = trailer_index {
            trailer_index
        } else {
            return Err(Error::InvalidArchiveError("Archive missing trailer.".to_string()))
        };

        let trailer_format: CpioFormat = if let Some(trailer_format) = trailer_format {
            trailer_format
        } else {
            return Err(Error::InvalidArchiveError("Archive missing trailer.".to_string()))
        };

        let mut dat = self.mem[..trailer_index].to_vec();
        dat.append(&mut entry_bytes(fs_path, internal_path, dat.len(), trailer_format, None)?);
        dat.append(&mut trailer_bytes(trailer_format));

        // pad to 0x100 alignment
        let mut padding = vec![];
        if dat.len() % 100 != 0 {
            padding.resize(4 - (dat.len() % 4), 0)
        }
        dat.append(&mut padding);

        let mut out_fp = File::create(archive_path).map_err(|_|
            Error::FileSystemError(
                format!("Failed to create output file {}", archive_path.to_string_lossy())
            )
        )?;
        out_fp.write(&dat).map_err(|_|
            Error::FileSystemError(String::from("failed to write data to archive file"))
        )?;

        Ok(())

    }

    pub fn unarchive(&self, output_path: &Path) -> Result<(), Error> {
        let output_path = output_path.canonicalize().map_err(|e| {
            Error::FileSystemError(e.to_string())
        })?;
        if !output_path.exists() {
            create_dir(&output_path).map_err(|_|
                Error::FileSystemError(
                    format!("Unable to create output directory: {}", output_path.display())
                )
            )?
        }
        let mut iter = self.iter_files();
        while let Some(file) = iter.next()? {
            if !file.is_trailer()? {
                self.extract_one(&output_path, &file)?
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CpioEntryHeader<'a> {
    c_magic     : &'a[u8],
    c_ino       : &'a[u8],
    c_mode      : &'a[u8],
    c_uid       : &'a[u8],
    c_gid       : &'a[u8],
    c_nlink     : &'a[u8],
    c_mtime     : &'a[u8],
    c_filesize  : &'a[u8],
    c_devmajor  : &'a[u8],
    c_devminor  : &'a[u8],
    c_rdevmajor : &'a[u8],
    c_rdevminor : &'a[u8],
    c_namesize  : &'a[u8],
    c_check     : &'a[u8],
}

#[derive(Debug)]
pub struct CpioEntry<'a> {
    /// Offset into the archive of this file entry
    pub index: usize,

    /// Which Cpio format is used
    format: CpioFormat,

    /// Memory of the cpio file
    mem: &'a [u8],

    /// Parsed header of the cpio entry
    header: CpioEntryHeader<'a>
}

impl<'a> CpioEntry<'a> {
    pub(crate) fn new(index: usize, format: CpioFormat, mem: &'a [u8])
    -> Result<Self, Error> {
        if mem.len() - index < CPIO_HEADER_LEN {
            return Err(Error::EarlyEOFError);
        }

        #[allow(clippy::identity_op)]
        #[allow(clippy::erasing_op)]
        let header = CpioEntryHeader {
            c_magic     : &mem[index..index+CPIO_MAGIC_LEN],
            c_ino       : &mem[index+CPIO_MAGIC_LEN+( 0*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 1*CPIO_FIELD_LEN)],
            c_mode      : &mem[index+CPIO_MAGIC_LEN+( 1*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 2*CPIO_FIELD_LEN)],
            c_uid       : &mem[index+CPIO_MAGIC_LEN+( 2*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 3*CPIO_FIELD_LEN)],
            c_gid       : &mem[index+CPIO_MAGIC_LEN+( 3*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 4*CPIO_FIELD_LEN)],
            c_nlink     : &mem[index+CPIO_MAGIC_LEN+( 4*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 5*CPIO_FIELD_LEN)],
            c_mtime     : &mem[index+CPIO_MAGIC_LEN+( 5*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 6*CPIO_FIELD_LEN)],
            c_filesize  : &mem[index+CPIO_MAGIC_LEN+( 6*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 7*CPIO_FIELD_LEN)],
            c_devmajor  : &mem[index+CPIO_MAGIC_LEN+( 7*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 8*CPIO_FIELD_LEN)],
            c_devminor  : &mem[index+CPIO_MAGIC_LEN+( 8*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+( 9*CPIO_FIELD_LEN)],
            c_rdevmajor : &mem[index+CPIO_MAGIC_LEN+( 9*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+(10*CPIO_FIELD_LEN)],
            c_rdevminor : &mem[index+CPIO_MAGIC_LEN+(10*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+(11*CPIO_FIELD_LEN)],
            c_namesize  : &mem[index+CPIO_MAGIC_LEN+(11*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+(12*CPIO_FIELD_LEN)],
            c_check     : &mem[index+CPIO_MAGIC_LEN+(12*CPIO_FIELD_LEN)..index+CPIO_MAGIC_LEN+(13*CPIO_FIELD_LEN)],
        };

        Ok(Self { index, format, mem, header })
    }

    pub fn magic(&self) -> &[u8] {
        self.header.c_magic
    }

    pub fn inode(&self) -> Result<u64, Error> {
        let str_inode = from_utf8(self.header.c_ino).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_ino' from utf8 failed"))
        )?;

        u64::from_str_radix(str_inode, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_ino' to u64 failed"))
        })
    }

    pub fn mode(&self) -> Result<u64, Error> {
        let str_mode = from_utf8(self.header.c_mode).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_mode' from utf8 failed"))
        )?;

        u64::from_str_radix(str_mode, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_mode' to u64 failed"))
        })
    }

    pub fn mode_str(&self) -> Result<String, Error> {
        mode_to_str(self.mode()?)
    }

    pub fn is_link(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFLNK)
    }

    pub fn is_dir(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFDIR)
    }

    pub fn is_reg(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFREG)
    }

    pub fn is_sock(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFSOCK)
    }

    pub fn is_fifo(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFIFO)
    }

    pub fn is_blk(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFBLK)
    }

    pub fn is_chr(&self) -> Result<bool, Error> {
        Ok((self.mode()? & defs::S_IFMT) == defs::S_IFCHR)
    }

    pub fn uid(&self) -> Result<u64, Error> {
        let str_uid = from_utf8(self.header.c_uid).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_uid' from utf8 failed"))
        )?;

        u64::from_str_radix(str_uid, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_uid' to u64 failed"))
        })
    }

    pub fn gid(&self) -> Result<u64, Error> {
        let str_gid = from_utf8(self.header.c_gid).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_gid' from utf8 failed"))
        )?;

        u64::from_str_radix(str_gid, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_gid' to u64 failed"))
        })
    }

    pub fn nlink(&self) -> Result<u64, Error> {
        let str_nlink = from_utf8(self.header.c_nlink).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_nlink' from utf8 failed"))
        )?;

        u64::from_str_radix(str_nlink, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_nlink' to u64 failed"))
        })
    }

    pub fn mtime(&self) -> Result<u64, Error> {
        let str_mtime = from_utf8(self.header.c_mtime).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_mtime' from utf8 failed"))
        )?;

        u64::from_str_radix(str_mtime, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_mtime' to u64 failed"))
        })
    }

    pub fn filesize(&self) -> Result<usize, Error> {
        let str_filesize = from_utf8(self.header.c_filesize).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_filesize' from utf8 failed"))
        )?;

        usize::from_str_radix(str_filesize, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_filesize' to usize failed"))
        })
    }


    /// The file content begins after the name, the start is 4-byte aligned
    fn file_content_offset(&self) -> Result<usize, Error> {
        let nsize = self.namesize()?;
        let noff = self.name_offset();

        let mut nend = noff + nsize;
        if (self.index + nend) % 4 != 0 {
            nend += 4 - ((self.index + nend) % 4);
        }

        Ok(nend)
    }

    pub fn file_content(&self) -> Result<&[u8], Error> {
        let fc_start = self.file_content_offset()?;
        let fc_size = self.filesize()?;

        let slice = &self.mem[self.index..];

        if fc_start + fc_size >= slice.len() {
            Err(Error::EarlyEOFError)
        } else {
            Ok(&slice[fc_start..fc_start+fc_size])
        }
    }

    pub fn devmajor(&self) -> Result<u64, Error> {
        let str_devmajor = from_utf8(self.header.c_devmajor).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_devmajor' from utf8 failed"))
        )?;

        u64::from_str_radix(str_devmajor, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_devmajor' to u64 failed"))
        })
    }

    pub fn devminor(&self) -> Result<u64, Error> {
        let str_devminor = from_utf8(self.header.c_devminor).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_devminor' from utf8 failed"))
        )?;

        u64::from_str_radix(str_devminor, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_devminor' to u64 failed"))
        })
    }

    pub fn rdevmajor(&self) -> Result<u64, Error> {
        let str_rdevmajor = from_utf8(self.header.c_rdevmajor).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_rdevmajor' from utf8 failed"))
        )?;

        u64::from_str_radix(str_rdevmajor, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_rdevmajor' to u64 failed"))
        })
    }

    pub fn rdevminor(&self) -> Result<u64, Error> {
        let str_rdevminor = from_utf8(self.header.c_rdevminor).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_rdevminor' from utf8 failed"))
        )?;

        u64::from_str_radix(str_rdevminor, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_rdevminor' to u64 failed"))
        })
    }

    pub fn namesize(&self) -> Result<usize, Error> {
        let str_namesize = from_utf8(self.header.c_namesize).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_namesize' from utf8 failed"))
        )?;

        usize::from_str_radix(str_namesize, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_namesize' to usize failed"))
        })
    }

    /// The name starts immediately after the header
    fn name_offset(&self) -> usize {
        CPIO_HEADER_LEN
    }

    pub fn name(&self) -> Result<&[u8], Error> {
        let nsize = self.namesize()?;
        let noff = self.name_offset();
        let slice = &self.mem[self.index..];
        if self.name_offset() + nsize > slice.len() {
            Err(Error::EarlyEOFError)
        } else {
            Ok(&slice[noff..noff+nsize])
        }
    }

    pub fn checksum(&self) -> Result<u64, Error> {
        let str_check = from_utf8(self.header.c_check).map_err(|_|
            Error::EntryConversionError(String::from("Converting 'c_check' from utf8 failed"))
        )?;

        u64::from_str_radix(str_check, 16).map_err(|_| {
            Error::EntryConversionError(String::from("Converting 'c_check' to u64 failed"))
        })
    }

    pub fn is_trailer(&self) -> Result<bool, Error> {
        Ok(self.namesize()? == 0xb && self.name()? == b"TRAILER!!!\0")
    }

    /// The next entry ends after the file content, the start is 4-byte aligned
    pub fn next(&self) -> Result<usize, Error> {
        let mut next_offset = self.index + self.file_content_offset()? + self.filesize()?;
        if next_offset % 4 != 0 {
            next_offset += 4 - (next_offset % 4);
        }
        Ok(next_offset)
    }

    pub fn valid_magic(&self) -> Result<bool, Error> {
        if self.mem.len() - self.index < defs::CPIO_MAGIC_LEN {
            return Err(Error::EarlyEOFError);
        }

        let slice = &self.mem[self.index..];

        let is_valid = match self.format {
            CpioFormat::Newc => {
                slice.starts_with(defs::NEWC_MAGIC)
            },
            CpioFormat::Crc => {
                slice.starts_with(defs::CRC_MAGIC)
            }
        };

        Ok(is_valid)
    }
}

pub struct CpioEntryIter<'a> {
    /// Offset into the archive of the current entry
    index: usize,

    /// Memory of the archive file
    archive_mem: &'a [u8],

    /// Expected format of entries
    format: CpioFormat,

    /// Trailer was encountered
    trailer_seen: bool,
}

impl<'a> FallibleIterator for CpioEntryIter<'a> {
    type Item = CpioEntry<'a>;
    type Error = Error;

    fn next(&mut self) -> Result<Option<Self::Item>, Self::Error> {
        if self.trailer_seen {
            return Ok(None)
        }

        if self.index > self.archive_mem.len() {
            return Err(Error::EarlyEOFError)
        }

        let file = CpioEntry::new(
            self.index,
            self.format,
            self.archive_mem,
        )?;

        if file.is_trailer()? {
            self.trailer_seen = true;
        }

        if !file.valid_magic()? {
            return Err(Error::InvalidArchiveError(
                String::from("Invalid magic encountered")
            ))
        }

        self.index = file.next()?;

        Ok(Some(file))
    }
}

/// helper function to enumerate file paths in a directory
fn collect_files(dir: &PathBuf) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .collect()
}

/// Creates a CPIO archive from the directory in `directory_path` and write the
/// created archive to `output_path`, using the specified `format`, and optionally
/// gzip compresses the archive according to the `gzip` argument.
pub fn archive_directory(
    directory_path: &PathBuf,
    output_path: &PathBuf,
    format: CpioFormat,
    gzip: bool
) -> Result<(), Error> {
    let mut builder = CpioBuilder::new(format);

    let files = collect_files(directory_path);
    for file in files {
        if let Some(file_str) = file.to_str() {
            if let Some(directory_path_str) = directory_path.to_str() {
                let internal_path = file_str
                    .trim_start_matches(directory_path_str)
                    .trim_start_matches('/');
                // println!("{}", &internal_path);
                builder.insert(&file, internal_path)?;
            }
        }
    }
    builder.write(output_path, gzip)?;
    Ok(())
}
