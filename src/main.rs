use fallible_iterator::FallibleIterator;
use clap::{Parser, Subcommand};
use memmap2::Mmap;

use std::path::PathBuf;
use std::process::exit;
use std::io::Write;
use std::fs::File;

use rcpio::Cpio;

type Result<T> = anyhow::Result<T>;

#[derive(Parser)]
struct CmdArgs {
    #[clap(subcommand)]
    commands: Commands
}

#[derive(Subcommand)]
enum Commands {
    /// Create a cpio archive from a directory
    Ar {
        /// Path to the directory to archive
        directory_path: PathBuf,

        /// Output path for created archive
        output_path: PathBuf,

        /// Use the SVR4 CRC format (default is no CRC)
        #[clap(short='c', long, action)]
        crc: bool,

        /// Compress the archive in gzip format
        #[clap(short='g', long, action)]
        gzip: bool
    },
    /// Extract a cpio archive to a directory
    Unar {
        /// Path to the directory to archive
        archive_path: PathBuf,

        /// Output path for extracted archive
        output_path: PathBuf,
    },
    // /// Merge two cpio archives to a single archive
    // Merge {

    // },
    /// Extract a single file from a cpio archive
    Cat {
        /// Path to the directory to archive
        archive_path: PathBuf,

        /// Path to the file to extract
        internal_path: String,
    },
    // /// Insert a single file into an existing cpio archive
    Push {
        /// Path to the directory to archive
        archive_path: PathBuf,

        /// Path to the file to extract
        insert_path: PathBuf,

        /// Path to the file to extract
        internal_path: String,
    },
    /// List the files in a cpio archive
    Ls {
        /// Path to the cpio archive to inspect
        archive_path: PathBuf,
    },
}

fn main() -> Result<()> {
    let args = CmdArgs::parse();
    match args.commands {
        Commands::Ar { directory_path, output_path, crc, gzip } => {
            let format = if crc {
                rcpio::CpioFormat::Crc
            } else {
                rcpio::CpioFormat::Newc
            };
            rcpio::archive_directory(&directory_path, &output_path, format, gzip)?;
        },
        Commands::Ls { archive_path } => {
            let archive = File::open(archive_path)?;
            let mmap = &*unsafe { Mmap::map(&archive) }?;

            let cpio = Cpio::load(mmap)?;

            let mut iter = cpio.iter_files();
            while let Some(file) = iter.next()? {
                if file.is_trailer()? {
                    break;
                }

                if file.is_link()? {
                    println!(
                        "{} {:>2} {:>4} {:>4} {:>8} {} -> {}",
                        file.mode_str()?,
                        file.nlink()?,
                        file.uid()?,
                        file.gid()?,
                        file.filesize()?,
                        std::str::from_utf8(file.name()?)?,
                        std::str::from_utf8(file.file_content()?)?,
                    );
                } else {
                    println!(
                        "{} {:>2} {:>4} {:>4} {:>8} {}",
                        file.mode_str()?,
                        file.nlink()?,
                        file.uid()?,
                        file.gid()?,
                        file.filesize()?,
                        std::str::from_utf8(file.name()?)?,
                    );
                }
            }
        },
        Commands::Cat { archive_path, internal_path } => {
            let archive = File::open(archive_path)?;
            let mmap = &*unsafe { Mmap::map(&archive) }?;

            let cpio = Cpio::load(mmap)?;

            let mut iter = cpio.iter_files();
            while let Some(file) = iter.next()? {
                if file.is_trailer()? {
                    break;
                }

                let name = String::from_utf8(file.name()?.to_vec())?;
                let trimmed_name = name.trim_end_matches('\0');

                if trimmed_name != internal_path {
                    continue;
                }

                if !file.mode_str()?.starts_with('-') {
                    eprintln!("Cat is only supported for regular files!");
                    exit(1);
                }

                std::io::stdout().write_all(file.file_content()?)?;
                return Ok(())
            }
            eprintln!("No file found in archive for path: '{internal_path}'");
            exit(1);
        },
        Commands::Push { archive_path, insert_path, internal_path } => {
            let archive = File::open(&archive_path)?;
            let mmap = &*unsafe { Mmap::map(&archive) }?;

            let cpio = Cpio::load(mmap)?;
            cpio.push(&archive_path, &insert_path, &internal_path)?;
        },
        Commands::Unar { archive_path, output_path } => {
            let archive = File::open(archive_path)?;
            let mmap = &*unsafe { Mmap::map(&archive) }?;

            let cpio = Cpio::load(mmap)?;
            cpio.unarchive(&output_path)?;
        },
    }

    Ok(())
}
