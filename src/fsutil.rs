use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

pub fn create_atomic(out: &Path) -> Result<NamedTempFile> {
    let dir = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".pakk-").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    builder
        .tempfile_in(dir)
        .with_context(|| format!("cannot create a file in {}", dir.display()))
}

pub fn persist(tmp: NamedTempFile, out: &Path) -> Result<()> {
    tmp.persist(out)
        .map(drop)
        .map_err(|e| e.error)
        .with_context(|| format!("cannot write {}", out.display()))
}

#[must_use]
pub fn is_macos_metadata(path: &Path) -> bool {
    path.components().any(|c| {
        let part = c.as_os_str().to_string_lossy();
        part == "__MACOSX" || part.starts_with("._")
    })
}

pub fn open_regular(path: &Path) -> io::Result<File> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no longer a regular file",
        ));
    }
    Ok(file)
}

pub fn create_file(path: &Path) -> io::Result<File> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(path)?;
    }
    File::create(path)
}

pub fn set_mtime(path: &Path, mtime: SystemTime) -> io::Result<()> {
    filetime::set_symlink_file_times(
        path,
        filetime::FileTime::now(),
        filetime::FileTime::from_system_time(mtime),
    )
}

#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o1777))
}

#[cfg(not(unix))]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o222 == 0);
    fs::set_permissions(path, perms)
}

#[derive(Debug, Clone)]
pub struct SharedFile {
    file: Arc<File>,
    len: u64,
    pos: u64,
}

impl SharedFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            file: Arc::new(file),
            len,
            pos: 0,
        })
    }

    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Read for SharedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(&*self.file, buf, self.pos)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(&*self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SharedFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = target
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek out of range"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn shared_file_clones_have_independent_cursors() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"0123456789").unwrap();
        let mut a = SharedFile::open(tmp.path()).unwrap();
        let mut b = a.clone();
        a.seek(SeekFrom::Start(6)).unwrap();
        let mut buf = [0; 4];
        a.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"6789");
        b.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"0123");
        assert_eq!(b.seek(SeekFrom::End(-1)).unwrap(), 9);
        assert!(b.seek(SeekFrom::Current(-100)).is_err());
    }

    #[test]
    fn atomic_file_only_appears_on_persist() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.zip");
        let mut tmp = create_atomic(&out).unwrap();
        tmp.write_all(b"data").unwrap();
        assert!(!out.exists());
        persist(tmp, &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"data");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_refuses_symlinks_and_fifos() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(open_regular(&file).is_ok());

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(open_regular(&link).is_err());

        let fifo = dir.path().join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(open_regular(&fifo).is_err());
    }

    #[test]
    fn recognises_macos_metadata() {
        assert!(is_macos_metadata(Path::new("__MACOSX/a/._b")));
        assert!(is_macos_metadata(Path::new("dir/._file.txt")));
        assert!(!is_macos_metadata(Path::new("dir/.file")));
        assert!(!is_macos_metadata(Path::new("MACOSX/file")));
    }

    #[cfg(unix)]
    #[test]
    fn create_file_replaces_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        create_file(&link).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
