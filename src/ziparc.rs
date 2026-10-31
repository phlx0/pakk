use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use zip::write::{SimpleFileOptions, ZipFileBuilder};
use zip::{CompressionMethod, PreparedZipFile, ZipArchive, ZipWriter};

use crate::fsutil::{self, SharedFile};
use crate::listing::Listing;
use crate::progress::{Progress, Summary};
use crate::walk::{self, Entry, Kind};

const BATCH_BYTES: u64 = 64 << 20;
const BATCH_ENTRIES: usize = 4096;
const STREAM_THRESHOLD: u64 = 128 << 20;
const PRECOMPRESSED: &[&str] = &[
    "7z", "aac", "apk", "avif", "br", "bz2", "docx", "epub", "flac", "gif", "gz", "heic", "jar",
    "jpeg", "jpg", "lz4", "m4a", "m4v", "mkv", "mov", "mp3", "mp4", "odt", "ogg", "opus", "png",
    "pptx", "rar", "tgz", "webm", "webp", "whl", "woff", "woff2", "xlsx", "xz", "zip", "zst",
];

enum Item<'a> {
    Prepared(PreparedZipFile),
    Stream(&'a Entry, SimpleFileOptions),
    Dir(&'a str, SimpleFileOptions),
    Symlink(&'a str, String, SimpleFileOptions),
}

pub fn pack<W: Write + Seek + Send>(
    entries: &[Entry],
    out: W,
    level: u32,
    progress: &Progress,
) -> Result<(W, Summary)> {
    let (tx, rx) = sync_channel(1);
    thread::scope(|s| {
        let writer = s.spawn(move || write_items(ZipWriter::new(out), &rx, progress));
        let produced = produce(entries, level, progress, &tx);
        drop(tx);
        let written = writer.join().map_err(|_| anyhow!("zip writer panicked"))?;
        produced?;
        written
    })
}

fn produce<'a>(
    entries: &'a [Entry],
    level: u32,
    progress: &Progress,
    tx: &SyncSender<Vec<Item<'a>>>,
) -> Result<()> {
    for batch in walk::batches(entries, BATCH_BYTES, BATCH_ENTRIES, STREAM_THRESHOLD) {
        let items = batch
            .par_iter()
            .map(|entry| prepare(entry, level, progress))
            .collect::<Result<Vec<_>>>()?;
        if tx.send(items).is_err() {
            break;
        }
    }
    Ok(())
}

fn prepare<'a>(entry: &'a Entry, level: u32, progress: &Progress) -> Result<Item<'a>> {
    let base = SimpleFileOptions::default()
        .last_modified_time(to_dos_time(entry.mtime))
        .unix_permissions(entry.mode);
    let size = match &entry.kind {
        Kind::Dir => return Ok(Item::Dir(&entry.name, base)),
        Kind::Symlink { target } => {
            let target = target.to_string_lossy().replace('\\', "/");
            return Ok(Item::Symlink(&entry.name, target, base));
        }
        Kind::File { size } => *size,
    };

    let deflate = level > 0 && !is_precompressed(&entry.name);
    let options = if deflate {
        base.compression_method(CompressionMethod::Deflated)
            .compression_level(Some(i64::from(level)))
    } else {
        base.compression_method(CompressionMethod::Stored)
    };
    if size >= STREAM_THRESHOLD {
        return Ok(Item::Stream(
            entry,
            options.large_file(size > u64::from(u32::MAX)),
        ));
    }

    let mut prepared = compress(entry, options)?;
    if deflate && prepared.compressed_size() >= prepared.uncompressed_size() {
        prepared = compress(entry, base.compression_method(CompressionMethod::Stored))?;
    }
    progress.inc(size);
    Ok(Item::Prepared(prepared))
}

fn compress(entry: &Entry, options: SimpleFileOptions) -> Result<PreparedZipFile> {
    let mut file = fsutil::open_regular(&entry.path)
        .with_context(|| format!("cannot open {}", entry.path.display()))?;
    let mut builder = ZipFileBuilder::new(&entry.name, options)?;
    io::copy(&mut file, &mut builder)
        .with_context(|| format!("cannot read {}", entry.path.display()))?;
    Ok(builder.finish()?)
}

fn write_items<W: Write + Seek>(
    mut zip: ZipWriter<W>,
    rx: &Receiver<Vec<Item<'_>>>,
    progress: &Progress,
) -> Result<(W, Summary)> {
    let mut summary = Summary::default();
    for item in rx.iter().flatten() {
        match item {
            Item::Prepared(file) => {
                summary.files += 1;
                summary.bytes += file.uncompressed_size();
                zip.add_prepared_file(file)?;
            }
            Item::Stream(entry, options) => {
                zip.start_file(&entry.name, options)?;
                let file = fsutil::open_regular(&entry.path)
                    .with_context(|| format!("cannot open {}", entry.path.display()))?;
                summary.files += 1;
                summary.bytes += io::copy(&mut progress.wrap_read(file), &mut zip)
                    .with_context(|| format!("cannot read {}", entry.path.display()))?;
            }
            Item::Dir(name, options) => {
                summary.dirs += 1;
                zip.add_directory(name, options)?;
            }
            Item::Symlink(name, target, options) => {
                summary.symlinks += 1;
                zip.add_symlink(name, target, options)?;
            }
        }
    }
    Ok((zip.finish()?, summary))
}

fn is_precompressed(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        PRECOMPRESSED
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
    })
}

struct Planned {
    index: usize,
    path: PathBuf,
    mode: Option<u32>,
    mtime: Option<SystemTime>,
}

#[derive(Default)]
struct Plan {
    files: Vec<Planned>,
    dirs: Vec<Planned>,
    symlinks: Vec<Planned>,
    parents: BTreeSet<PathBuf>,
    bytes: u64,
}

pub fn unpack(archive: &Path, dest: &Path, progress: &Progress) -> Result<Summary> {
    let mut zip = open(archive)?;
    let plan = plan(&mut zip)?;
    progress.set_total(plan.bytes);

    for dir in &plan.parents {
        let dir = dest.join(dir);
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    plan.files.par_iter().try_for_each_init(
        || zip.clone(),
        |zip, file| extract_file(zip, file, dest, progress),
    )?;
    for link in &plan.symlinks {
        extract_symlink(&mut zip, link, dest)?;
    }
    for dir in plan.dirs.iter().rev() {
        restore_metadata(&dest.join(&dir.path), dir)?;
    }

    Ok(Summary {
        files: plan.files.len() as u64,
        dirs: plan.dirs.len() as u64,
        symlinks: plan.symlinks.len() as u64,
        bytes: plan.bytes,
    })
}

pub fn list(archive: &Path) -> Result<Vec<Listing>> {
    let mut zip = open(archive)?;
    (0..zip.len())
        .map(|i| {
            let file = zip.by_index_raw(i)?;
            let kind = if file.is_dir() {
                Kind::Dir
            } else if file.is_symlink() {
                Kind::Symlink {
                    target: PathBuf::new(),
                }
            } else {
                Kind::File { size: file.size() }
            };
            Ok(Listing {
                name: file.name()?.trim_end_matches('/').to_owned(),
                kind,
                mtime: file.last_modified().and_then(from_dos_time),
            })
        })
        .collect()
}

fn open(archive: &Path) -> Result<ZipArchive<SharedFile>> {
    let file =
        SharedFile::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid zip archive", archive.display()))
}

fn plan(zip: &mut ZipArchive<SharedFile>) -> Result<Plan> {
    let mut plan = Plan::default();
    for index in 0..zip.len() {
        let file = zip.by_index_raw(index)?;
        if file.encrypted() {
            bail!(
                "`{}` is encrypted, which pakk does not support",
                file.name()?
            );
        }
        let path = file.enclosed_name().ok_or_else(|| {
            anyhow!(
                "refusing to extract unsafe path `{}`",
                String::from_utf8_lossy(file.name_raw())
            )
        })?;
        if path.as_os_str().is_empty() || fsutil::is_macos_metadata(&path) {
            continue;
        }
        let planned = Planned {
            index,
            mode: file.unix_mode(),
            mtime: file.last_modified().and_then(from_dos_time),
            path,
        };
        if file.is_dir() {
            plan.parents.insert(planned.path.clone());
            plan.dirs.push(planned);
            continue;
        }
        if let Some(parent) = planned.path.parent() {
            plan.parents.insert(parent.to_path_buf());
        }
        if file.is_symlink() {
            plan.symlinks.push(planned);
        } else {
            plan.bytes += file.size();
            plan.files.push(planned);
        }
    }
    Ok(plan)
}

fn extract_file(
    zip: &mut ZipArchive<SharedFile>,
    file: &Planned,
    dest: &Path,
    progress: &Progress,
) -> Result<()> {
    let path = dest.join(&file.path);
    let mut reader = zip.by_index(file.index)?;
    let out =
        fsutil::create_file(&path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut writer = BufWriter::with_capacity(256 << 10, progress.wrap_write(out));
    io::copy(&mut reader, &mut writer)
        .and_then(|_| writer.flush())
        .with_context(|| format!("cannot extract {}", file.path.display()))?;
    drop(writer);
    restore_metadata(&path, file)
}

fn extract_symlink(zip: &mut ZipArchive<SharedFile>, link: &Planned, dest: &Path) -> Result<()> {
    let target = io::read_to_string(zip.by_index(link.index)?)?;
    let path = dest.join(&link.path);
    if fs::symlink_metadata(&path).is_ok() {
        fs::remove_file(&path).with_context(|| format!("cannot replace {}", path.display()))?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &path)
        .with_context(|| format!("cannot create symlink {}", path.display()))?;
    #[cfg(not(unix))]
    fs::write(&path, &target).with_context(|| format!("cannot create {}", path.display()))?;
    if let Some(mtime) = link.mtime {
        let _ = fsutil::set_mtime(&path, mtime);
    }
    Ok(())
}

fn restore_metadata(path: &Path, item: &Planned) -> Result<()> {
    if let Some(mode) = item.mode {
        fsutil::set_mode(path, mode)
            .with_context(|| format!("cannot set permissions on {}", path.display()))?;
    }
    if let Some(mtime) = item.mtime {
        fsutil::set_mtime(path, mtime)
            .with_context(|| format!("cannot set mtime on {}", path.display()))?;
    }
    Ok(())
}

fn to_dos_time(time: SystemTime) -> zip::DateTime {
    jiff::Timestamp::try_from(time)
        .ok()
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()).datetime())
        .and_then(|dt| zip::DateTime::try_from(dt).ok())
        .unwrap_or_default()
}

fn from_dos_time(time: zip::DateTime) -> Option<SystemTime> {
    let civil = jiff::civil::DateTime::try_from(time).ok()?;
    let zoned = civil.to_zoned(jiff::tz::TimeZone::system()).ok()?;
    Some(zoned.timestamp().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Cursor;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        fs::create_dir_all(root.join("nested/empty")).unwrap();
        fs::write(root.join("text.txt"), "hello world ".repeat(5000)).unwrap();
        fs::write(root.join("photo.jpg"), [0xff, 0xd8, 0xff, 0xe0]).unwrap();
        fs::write(root.join("nested/zero"), "").unwrap();
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let noise: Vec<u8> = (0..50_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect();
        fs::write(root.join("nested/noise.bin"), noise).unwrap();
        dir
    }

    fn pack_to_vec(dir: &Path) -> Vec<u8> {
        let entries = walk::collect(&[dir.join("src")], None).unwrap().entries;
        let (cursor, summary) =
            pack(&entries, Cursor::new(Vec::new()), 6, &Progress::hidden()).unwrap();
        assert_eq!(summary.files, 4);
        assert_eq!(summary.dirs, 3);
        cursor.into_inner()
    }

    #[test]
    fn roundtrips_tree() {
        let dir = fixture();
        let bytes = pack_to_vec(dir.path());
        let archive = dir.path().join("out.zip");
        fs::write(&archive, bytes).unwrap();

        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let summary = unpack(&archive, &dest, &Progress::hidden()).unwrap();
        assert_eq!(summary.files, 4);

        for rel in ["text.txt", "photo.jpg", "nested/zero", "nested/noise.bin"] {
            assert_eq!(
                fs::read(dir.path().join("src").join(rel)).unwrap(),
                fs::read(dest.join("src").join(rel)).unwrap(),
                "{rel}"
            );
        }
        assert!(dest.join("src/nested/empty").is_dir());
    }

    #[test]
    fn picks_compression_per_file() {
        let dir = fixture();
        let mut zip = ZipArchive::new(Cursor::new(pack_to_vec(dir.path()))).unwrap();
        let method = |zip: &mut ZipArchive<_>, name| zip.by_name(name).unwrap().compression();
        assert_eq!(
            method(&mut zip, "src/text.txt"),
            CompressionMethod::Deflated
        );
        assert_eq!(method(&mut zip, "src/photo.jpg"), CompressionMethod::Stored);
        assert_eq!(
            method(&mut zip, "src/nested/noise.bin"),
            CompressionMethod::Stored
        );
    }

    #[test]
    fn output_is_deterministic_in_order() {
        let dir = fixture();
        let zip = ZipArchive::new(Cursor::new(pack_to_vec(dir.path()))).unwrap();
        let names: Vec<_> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        assert_eq!(
            names,
            [
                "src/",
                "src/nested/",
                "src/nested/empty/",
                "src/nested/noise.bin",
                "src/nested/zero",
                "src/photo.jpg",
                "src/text.txt"
            ]
        );
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        zip.start_file("../escape.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"pwned").unwrap();
        zip.finish().unwrap();

        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let err = unpack(&archive, &dest, &Progress::hidden()).unwrap_err();
        assert!(err.to_string().contains("unsafe path"), "{err}");
        assert!(!dir.path().join("escape.txt").exists());
    }
}
