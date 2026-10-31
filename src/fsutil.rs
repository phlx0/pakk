use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
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
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
        fs::remove_file(path)?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
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

#[derive(Debug)]
pub struct Beneath {
    root: PathBuf,
    verified: HashSet<PathBuf>,
}

impl Beneath {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            verified: HashSet::new(),
        }
    }

    pub fn create_parent(&mut self, rel: &Path) -> Result<()> {
        match rel.parent() {
            Some(parent) => self.create_dirs(parent),
            None => Ok(()),
        }
    }

    pub fn create_dirs(&mut self, rel: &Path) -> Result<()> {
        let mut current = PathBuf::new();
        for part in rel.components() {
            current.push(part);
            if self.verified.contains(&current) {
                continue;
            }
            let full = self.root.join(&current);
            match fs::symlink_metadata(&full) {
                Ok(meta) if meta.is_dir() => {}
                Ok(meta) if meta.file_type().is_symlink() => {
                    bail!(
                        "refusing to extract through existing symlink {}",
                        full.display()
                    );
                }
                Ok(_) => {
                    fs::remove_file(&full)
                        .and_then(|()| fs::create_dir(&full))
                        .with_context(|| {
                            format!("cannot replace {} with a directory", full.display())
                        })?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&full) {
                    Err(e) if !(e.kind() == io::ErrorKind::AlreadyExists && full.is_dir()) => {
                        return Err(e).with_context(|| format!("cannot create {}", full.display()));
                    }
                    _ => {}
                },
                Err(e) => {
                    return Err(e).with_context(|| format!("cannot inspect {}", full.display()));
                }
            }
            self.verified.insert(current.clone());
        }
        Ok(())
    }
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
    fn create_file_never_writes_through_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        let link = dir.path().join("link");
        fs::hard_link(&victim, &link).unwrap();
        create_file(&link).unwrap().write_all(b"new").unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert_eq!(fs::read(&link).unwrap(), b"new");
    }

    #[test]
    fn create_file_replaces_read_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ro");
        fs::write(&file, "old").unwrap();
        let mut perms = fs::metadata(&file).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&file, perms).unwrap();
        create_file(&file).unwrap().write_all(b"new").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn beneath_refuses_existing_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(dir.path().join("outside")).unwrap();
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("../outside", root.join("lnk")).unwrap();

        let mut beneath = Beneath::new(&root);
        beneath.create_dirs(Path::new("a/b/c")).unwrap();
        assert!(root.join("a/b/c").is_dir());
        let err = beneath.create_parent(Path::new("lnk/x/file")).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!dir.path().join("outside/x").exists());
    }

    #[test]
    fn beneath_replaces_files_with_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "file").unwrap();
        Beneath::new(dir.path())
            .create_dirs(Path::new("a/b"))
            .unwrap();
        assert!(dir.path().join("a/b").is_dir());
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
