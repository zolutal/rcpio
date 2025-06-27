use rcpio::{Cpio, CpioBuilder, CpioFormat};
use tempdir::TempDir;
use std::fs::{create_dir, read_link, set_permissions, symlink_metadata, File, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::process::Command;
use std::path::{Path, PathBuf};
use hexdump::hexdump;

fn collect_files(dir: &PathBuf) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .collect()
}

fn cpio_archive(path: &Path) -> Vec<u8> {
    let cpio_out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd {}; find . -print0 | cpio --null -o --format=crc",
             path.to_str().expect("Could not convert path to string")
         ))
        .output()
        .expect("Failed to execute cpio command");
    cpio_out.stdout
}

fn rcpio_archive(tmpdir_path: &Path, archive_dir: &PathBuf) -> Result<Vec<u8>, rcpio::Error> {
    let mut builder = CpioBuilder::new(CpioFormat::Crc);

    let files = collect_files(archive_dir);
    for file in files {
        if let Some(file_str) = file.to_str() {
            if let Some(directory_path_str) = archive_dir.to_str() {
                let internal_path = file_str
                    .trim_start_matches(directory_path_str)
                    .trim_start_matches('/');
                builder.insert(&file, internal_path)?;
            }
        }
    }

    let cpio_path = tmpdir_path.join("out.cpio");

    builder.write(&cpio_path, false)?;
    assert!(cpio_path.exists());

    let mut archive = File::open(&cpio_path).expect("Could not open cpio file");
    let mut archive_data = vec![];
    archive.read_to_end(&mut archive_data).expect("Failed to read file");

    Ok(archive_data)
}

fn test_compat<F>(tmpdir_path: &Path, f: F) -> Result<Vec<u8>, rcpio::Error>
where F: Fn(&Path) {
    let archive_dir = tmpdir_path.join("archive");
    create_dir(&archive_dir).expect("Failed to create directory");

    f(&archive_dir);

    let cpio_archive_data = cpio_archive(&archive_dir);
    let rcpio_archive_data = rcpio_archive(tmpdir_path, &archive_dir)?;

    println!("==== cpio hexdump =====");
    hexdump(&cpio_archive_data);
    println!("==== rcpio hexdump ====");
    hexdump(&rcpio_archive_data);

    assert_eq!(cpio_archive_data, rcpio_archive_data);

    Ok(rcpio_archive_data)
}

#[test]
fn test_cpio_compat() -> Result<(), rcpio::Error> {
    let tmpdir = TempDir::new("rcpio-test").expect("Could not create temp directory");
    let tmpdir_path = tmpdir.path();

    let res = test_compat(tmpdir_path, |archive_dir: &Path| {
        // Test Directories
        let test_dir = archive_dir.join("dir");
        create_dir(&test_dir).expect("Failed to create directory");

        // Test Regular Files
        let test_file = test_dir.join("file");
        let mut fp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&test_file)
            .expect("Could not create file");
        fp.write_all(b"meow").expect("Failed to write to file");

        assert!(&test_file.exists());

        // Test File Permissions
        set_permissions(&test_file, Permissions::from_mode(0o777))
            .expect("Failed to set file permissions");

        // Test Symlink
        symlink("/dir/file", test_dir.join("link")).expect("Failed to create symlink");
    })?;

    let out_dir = tmpdir_path.join("unarchive");
    create_dir(&out_dir).expect("Failed to create directory");

    let cpio = Cpio::load(&res)?;
    cpio.unarchive(&out_dir)?;
    assert!(out_dir.join("dir").join("file").exists());

    let fp = std::fs::OpenOptions::new()
        .read(true)
        .open(out_dir.join("dir").join("file"))
        .expect("Could not open file");
    let meta = fp.metadata().expect("Failed to get metadata for file");
    assert!(meta.mode() == 0o100777);

    let symlink_meta = symlink_metadata(out_dir.join("dir").join("link")).expect("Failed to get symlink metadata");
    assert!(symlink_meta.is_symlink());

    let link_target = read_link(out_dir.join("dir").join("link")).expect("Failed to read link");
    assert_eq!(link_target, PathBuf::from("/dir/file"));

    tmpdir.close().expect("Failed to close tempdir");

    Ok(())
}
