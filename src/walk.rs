use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    File { size: u64 },
    Dir,
    Symlink { target: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub mode: u32,
    pub mtime: SystemTime,
}

impl Entry {
    #[must_use]
    pub fn size(&self) -> u64 {
        match self.kind {
            Kind::File { size } => size,
            _ => 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct Collected {
    pub entries: Vec<Entry>,
    pub skipped: Vec<PathBuf>,
}

pub fn collect(inputs: &[PathBuf], exclude: Option<&Path>) -> Result<Collected> {
    let exclude = exclude.and_then(|p| fs::canonicalize(p).ok());
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    let mut skipped = Vec::new();

    for input in inputs {
        fs::symlink_metadata(input).with_context(|| format!("cannot read {}", input.display()))?;
        let prefix = if is_contents_only(input) {
            None
        } else {
            input.file_name().map(PathBuf::from)
        };

        for item in WalkDir::new(input).follow_links(false).sort_by_file_name() {
            let item = item.with_context(|| format!("cannot walk {}", input.display()))?;
            let rel = item.path().strip_prefix(input).unwrap_or(item.path());
            let name_path = match &prefix {
                Some(prefix) => prefix.join(rel),
                None if rel.as_os_str().is_empty() => continue,
                None => rel.to_path_buf(),
            };
            if is_excluded(item.path(), exclude.as_deref()) {
                continue;
            }

            let meta = item
                .metadata()
                .with_context(|| format!("cannot stat {}", item.path().display()))?;
            let Some(entry) = make_entry(item.path(), archive_name(&name_path)?, &meta)? else {
                skipped.push(item.path().to_path_buf());
                continue;
            };
            if !seen.insert(entry.name.clone()) {
                bail!("duplicate archive path `{}`", entry.name);
            }
            entries.push(entry);
        }
    }
    Ok(Collected { entries, skipped })
}

#[must_use]
pub fn batches(entries: &[Entry], max_bytes: u64, max_len: usize, cap: u64) -> Vec<&[Entry]> {
    let mut out = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (i, entry) in entries.iter().enumerate() {
        bytes += entry.size().min(cap);
        if bytes >= max_bytes || i + 1 - start >= max_len {
            out.push(&entries[start..=i]);
            (start, bytes) = (i + 1, 0);
        }
    }
    if start < entries.len() {
        out.push(&entries[start..]);
    }
    out
}

#[must_use]
pub fn total_size(entries: &[Entry]) -> u64 {
    entries.iter().map(Entry::size).sum()
}

fn is_contents_only(input: &Path) -> bool {
    let raw = input.as_os_str().to_string_lossy();
    raw == "." || raw.ends_with("/.") || (cfg!(windows) && raw.ends_with("\\."))
}

fn is_excluded(path: &Path, exclude: Option<&Path>) -> bool {
    exclude.is_some_and(|ex| {
        path.file_name() == ex.file_name() && fs::canonicalize(path).is_ok_and(|p| p == ex)
    })
}

fn archive_name(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        if let Component::Normal(part) = component {
            let Some(part) = part.to_str() else {
                bail!("cannot store non-UTF-8 file name {}", path.display());
            };
            parts.push(part);
        }
    }
    Ok(parts.join("/"))
}

fn make_entry(path: &Path, name: String, meta: &Metadata) -> Result<Option<Entry>> {
    let file_type = meta.file_type();
    let kind = if file_type.is_symlink() {
        let target =
            fs::read_link(path).with_context(|| format!("cannot read link {}", path.display()))?;
        Kind::Symlink { target }
    } else if file_type.is_dir() {
        Kind::Dir
    } else if file_type.is_file() {
        Kind::File { size: meta.len() }
    } else {
        return Ok(None);
    };
    Ok(Some(Entry {
        path: path.to_path_buf(),
        mode: mode(meta),
        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        name,
        kind,
    }))
}

#[cfg(unix)]
fn mode(meta: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode(meta: &Metadata) -> u32 {
    match (meta.is_dir(), meta.permissions().readonly()) {
        (true, _) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::write(root.join("README"), "hi").unwrap();
        fs::write(root.join("src/b.rs"), "b").unwrap();
        fs::write(root.join("src/a.rs"), "aaaa").unwrap();
        fs::write(root.join("src/deep/c.rs"), "").unwrap();
        dir
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn walks_directory_sorted_with_prefix() {
        let dir = tree();
        let entries = collect(&[dir.path().join("proj")], None).unwrap().entries;
        assert_eq!(
            names(&entries),
            [
                "proj",
                "proj/README",
                "proj/src",
                "proj/src/a.rs",
                "proj/src/b.rs",
                "proj/src/deep",
                "proj/src/deep/c.rs"
            ]
        );
        assert_eq!(total_size(&entries), 7);
    }

    #[test]
    fn dot_input_has_no_prefix() {
        let dir = tree();
        let entries = collect(&[dir.path().join("proj/src/.")], None)
            .unwrap()
            .entries;
        assert_eq!(names(&entries), ["a.rs", "b.rs", "deep", "deep/c.rs"]);
    }

    #[test]
    fn excludes_output_file() {
        let dir = tree();
        let out = dir.path().join("proj/README");
        let entries = collect(&[dir.path().join("proj")], Some(&out))
            .unwrap()
            .entries;
        assert!(!names(&entries).contains(&"proj/README"));
    }

    #[test]
    fn rejects_duplicate_names() {
        let dir = tree();
        let input = dir.path().join("proj/README");
        let err = collect(&[input.clone(), input], None).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn batches_respect_limits() {
        let entry = |size| Entry {
            path: PathBuf::new(),
            name: String::new(),
            kind: Kind::File { size },
            mode: 0o644,
            mtime: SystemTime::UNIX_EPOCH,
        };
        let entries: Vec<_> = (0..10).map(|_| entry(25)).collect();
        let sizes = |b: Vec<&[Entry]>| b.iter().map(|b| b.len()).collect::<Vec<_>>();
        assert_eq!(sizes(batches(&entries, 100, 99, u64::MAX)), [4, 4, 2]);
        assert_eq!(sizes(batches(&entries, 100, 3, u64::MAX)), [3, 3, 3, 1]);
        assert_eq!(sizes(batches(&entries, 100, 99, 10)), [10]);
        assert!(batches(&[], 100, 99, 10).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_non_utf8_names_instead_of_mangling_them() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tree();
        let bad = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
        fs::write(dir.path().join("proj").join(bad), "x").unwrap();
        let err = collect(&[dir.path().join("proj")], None).unwrap_err();
        assert!(err.to_string().contains("non-UTF-8"), "{err}");
    }

    #[test]
    fn missing_input_is_an_error() {
        assert!(collect(&[PathBuf::from("/definitely/not/here")], None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn skips_special_files() {
        let dir = tree();
        let fifo = dir.path().join("proj/pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let collected = collect(&[dir.path().join("proj")], None).unwrap();
        assert_eq!(collected.skipped, [fifo]);
        assert!(!names(&collected.entries).contains(&"proj/pipe"));
    }

    #[cfg(unix)]
    #[test]
    fn records_symlinks_without_following() {
        let dir = tree();
        std::os::unix::fs::symlink("README", dir.path().join("proj/link")).unwrap();
        let entries = collect(&[dir.path().join("proj")], None).unwrap().entries;
        let link = entries.iter().find(|e| e.name == "proj/link").unwrap();
        assert_eq!(
            link.kind,
            Kind::Symlink {
                target: "README".into()
            }
        );
    }
}
