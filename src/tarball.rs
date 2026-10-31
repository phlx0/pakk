use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Barrier, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use tar::{EntryType, Header, HeaderMode};

use crate::format::Format;
use crate::fsutil;
use crate::listing::Listing;
use crate::pgzip::{self, ParGzWriter};
use crate::progress::{Progress, Summary};
use crate::walk::{self, Entry, Kind};

const INLINE_LIMIT: u64 = 8 << 20;
const PREFETCH_BYTES: u64 = 64 << 20;
const PREFETCH_ENTRIES: usize = 4096;

enum Encoder<W: Write> {
    Plain(W),
    Gz(ParGzWriter<W>),
    Zst(zstd::Encoder<'static, W>),
}

impl<W: Write + Send + 'static> Encoder<W> {
    fn new(out: W, format: Format, level: u32, threads: usize) -> Result<Self> {
        Ok(match format {
            Format::Tar => Self::Plain(out),
            Format::TarGz => Self::Gz(ParGzWriter::new(out, level)),
            Format::TarZst => {
                let mut encoder = zstd::Encoder::new(out, i32::try_from(level)?)?;
                encoder.multithread(threads as u32)?;
                encoder.include_checksum(true)?;
                Self::Zst(encoder)
            }
            Format::Zip => bail!("zip is not a tar format"),
        })
    }

    fn finish(self) -> io::Result<W> {
        match self {
            Self::Plain(w) => Ok(w),
            Self::Gz(w) => w.finish(),
            Self::Zst(w) => w.finish(),
        }
    }
}

impl<W: Write + Send + 'static> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Gz(w) => w.write(buf),
            Self::Zst(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Gz(w) => w.flush(),
            Self::Zst(w) => w.flush(),
        }
    }
}

enum Loaded<'a> {
    Data(&'a Entry, fs::Metadata, Vec<u8>),
    Stream(&'a Entry),
    Path(&'a Entry),
}

pub fn pack<W: Write + Send + 'static>(
    entries: &[Entry],
    out: W,
    format: Format,
    level: u32,
    threads: usize,
    progress: &Progress,
) -> Result<(W, Summary)> {
    let encoder = Encoder::new(out, format, level, threads)?;
    let (tx, rx) = sync_channel(1);
    thread::scope(|s| {
        let writer = s.spawn(move || write_tar(encoder, &rx, progress));
        let loaded = prefetch(entries, &tx);
        drop(tx);
        let written = writer.join().map_err(|_| anyhow!("tar writer panicked"))?;
        loaded?;
        written
    })
}

fn prefetch<'a>(entries: &'a [Entry], tx: &SyncSender<Vec<Loaded<'a>>>) -> Result<()> {
    for batch in walk::batches(entries, PREFETCH_BYTES, PREFETCH_ENTRIES, INLINE_LIMIT) {
        let loaded = batch.par_iter().map(load).collect::<Result<Vec<_>>>()?;
        if tx.send(loaded).is_err() {
            break;
        }
    }
    Ok(())
}

fn load(entry: &Entry) -> Result<Loaded<'_>> {
    match entry.kind {
        Kind::File { size } if size <= INLINE_LIMIT => {
            let context = || format!("cannot read {}", entry.path.display());
            let mut file = fsutil::open_regular(&entry.path).with_context(context)?;
            let meta = file.metadata().with_context(context)?;
            let mut data = Vec::with_capacity(meta.len() as usize);
            file.read_to_end(&mut data).with_context(context)?;
            Ok(Loaded::Data(entry, meta, data))
        }
        Kind::File { .. } => Ok(Loaded::Stream(entry)),
        Kind::Dir | Kind::Symlink { .. } => Ok(Loaded::Path(entry)),
    }
}

fn write_tar<W: Write + Send + 'static>(
    encoder: Encoder<W>,
    rx: &Receiver<Vec<Loaded<'_>>>,
    progress: &Progress,
) -> Result<(W, Summary)> {
    let mut tar = tar::Builder::new(encoder);
    tar.mode(HeaderMode::Complete);
    tar.follow_symlinks(false);

    let mut summary = Summary::default();
    for item in rx.iter().flatten() {
        match item {
            Loaded::Data(entry, meta, data) => {
                let mut header = Header::new_gnu();
                header.set_metadata_in_mode(&meta, HeaderMode::Complete);
                header.set_size(data.len() as u64);
                tar.append_data(&mut header, &entry.name, data.as_slice())
                    .with_context(|| format!("cannot add {}", entry.path.display()))?;
                progress.inc(data.len() as u64);
                summary.files += 1;
                summary.bytes += data.len() as u64;
            }
            Loaded::Stream(entry) => {
                let context = || format!("cannot add {}", entry.path.display());
                let file = fsutil::open_regular(&entry.path).with_context(context)?;
                let meta = file.metadata().with_context(context)?;
                let mut header = Header::new_gnu();
                header.set_metadata_in_mode(&meta, HeaderMode::Complete);
                tar.append_data(&mut header, &entry.name, progress.wrap_read(file))
                    .with_context(context)?;
                summary.files += 1;
                summary.bytes += meta.len();
            }
            Loaded::Path(entry) => {
                tar.append_path_with_name(&entry.path, &entry.name)
                    .with_context(|| format!("cannot add {}", entry.path.display()))?;
                match entry.kind {
                    Kind::Dir => summary.dirs += 1,
                    _ => summary.symlinks += 1,
                }
            }
        }
    }
    let encoder = tar.into_inner().context("cannot finish tar stream")?;
    Ok((
        encoder.finish().context("cannot finish compression")?,
        summary,
    ))
}

fn open(
    archive: &Path,
    format: Format,
    progress: &Progress,
) -> Result<tar::Archive<Box<dyn Read + Send>>> {
    let file = File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    progress.set_total(file.metadata()?.len());
    let reader = BufReader::with_capacity(1 << 20, progress.wrap_read(file));
    let decoder: Box<dyn Read + Send> = match format {
        Format::Tar => Box::new(reader),
        Format::TarGz => pgzip::decoder(reader)?,
        Format::TarZst => Box::new(zstd::Decoder::with_buffer(reader)?),
        Format::Zip => bail!("zip is not a tar format"),
    };
    Ok(tar::Archive::new(decoder))
}

struct Job {
    path: PathBuf,
    data: Vec<u8>,
    meta: Meta,
}

#[derive(Clone, Copy)]
struct Meta {
    mode: Option<u32>,
    mtime: Option<SystemTime>,
}

enum Deferred {
    Symlink(PathBuf, PathBuf, Meta),
    HardLink(PathBuf, PathBuf),
}

enum Msg {
    Write(Job),
    Flush,
}

struct Pool<'a> {
    tx: SyncSender<Msg>,
    barrier: &'a Barrier,
    workers: usize,
}

impl Pool<'_> {
    fn write(&self, job: Job) -> bool {
        self.tx.send(Msg::Write(job)).is_ok()
    }

    fn flush(&self) -> bool {
        for _ in 0..self.workers {
            if self.tx.send(Msg::Flush).is_err() {
                return false;
            }
        }
        self.barrier.wait();
        true
    }
}

#[derive(Default)]
struct Links {
    items: Vec<Option<Deferred>>,
    by_path: HashMap<PathBuf, usize>,
}

impl Links {
    fn set(&mut self, rel: PathBuf, item: Deferred) {
        if let Some(&i) = self.by_path.get(&rel) {
            self.items[i] = Some(item);
        } else {
            self.by_path.insert(rel, self.items.len());
            self.items.push(Some(item));
        }
    }

    fn remove(&mut self, rel: &Path) {
        if let Some(i) = self.by_path.remove(rel) {
            self.items[i] = None;
        }
    }

    fn into_ordered(self) -> Vec<Deferred> {
        let mut items: Vec<_> = self.items.into_iter().flatten().collect();
        items.sort_by_key(|item| matches!(item, Deferred::Symlink(..)));
        items
    }
}

pub fn unpack(archive: &Path, dest: &Path, format: Format, progress: &Progress) -> Result<Summary> {
    let mut tar = open(archive, format, progress)?;
    let workers = rayon::current_num_threads().max(2);
    let (tx, rx) = sync_channel::<Msg>(workers * 4);
    let rx = Mutex::new(rx);
    let barrier = Barrier::new(workers + 1);
    let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);

    let read = thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while let Ok(msg) = rx.lock().map_err(drop).and_then(|r| r.recv().map_err(drop)) {
                    match msg {
                        Msg::Write(job) => {
                            if let Err(err) = write_job(&job) {
                                failure.lock().expect("lock").get_or_insert(err);
                            }
                        }
                        Msg::Flush => {
                            barrier.wait();
                        }
                    }
                }
            });
        }
        let pool = Pool {
            tx,
            barrier: &barrier,
            workers,
        };
        read_entries(&mut tar, dest, &pool, &failure)
    });

    if let Some(err) = failure.into_inner().expect("lock") {
        return Err(err);
    }
    let (summary, dirs, links) = read?;
    for item in links.into_ordered() {
        create_link(item)?;
    }
    for (dir, meta) in dirs.iter().rev() {
        restore(dir, meta)?;
    }
    Ok(summary)
}

type ReadResult = (Summary, Vec<(PathBuf, Meta)>, Links);

fn read_entries<R: Read>(
    tar: &mut tar::Archive<R>,
    dest: &Path,
    pool: &Pool<'_>,
    failure: &Mutex<Option<anyhow::Error>>,
) -> Result<ReadResult> {
    let mut summary = Summary::default();
    let mut dirs = Vec::new();
    let mut links = Links::default();
    let mut written = HashSet::new();
    let mut beneath = fsutil::Beneath::new(dest);

    for entry in tar.entries().context("cannot read archive")? {
        if failure.lock().expect("lock").is_some() {
            break;
        }
        let mut entry = entry.context("corrupt archive")?;
        let raw = entry.path()?.into_owned();
        let rel = safe_path(&raw)?;
        if rel.as_os_str().is_empty() || fsutil::is_macos_metadata(&rel) {
            continue;
        }
        let path = dest.join(&rel);
        let header = entry.header();
        let meta = Meta {
            mode: header.mode().ok(),
            mtime: header.mtime().ok().map(from_unix),
        };

        match header.entry_type() {
            EntryType::Directory => {
                beneath.create_dirs(&rel)?;
                dirs.push((path, meta));
                summary.dirs += 1;
            }
            EntryType::Symlink => {
                let target = entry.link_name()?.unwrap_or_default().into_owned();
                beneath.create_parent(&rel)?;
                links.set(rel, Deferred::Symlink(path, target, meta));
                summary.symlinks += 1;
            }
            EntryType::Link => {
                let target = safe_path(&entry.link_name()?.unwrap_or_default())?;
                beneath.create_parent(&target)?;
                let target = dest.join(target);
                beneath.create_parent(&rel)?;
                links.set(rel, Deferred::HardLink(path, target));
                summary.files += 1;
            }
            EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {
                beneath.create_parent(&rel)?;
                links.remove(&rel);
                if !written.insert(rel.clone()) && !pool.flush() {
                    break;
                }
                let size = entry.size();
                summary.files += 1;
                summary.bytes += size;
                if size > INLINE_LIMIT {
                    let out = fsutil::create_file(&path)
                        .with_context(|| format!("cannot create {}", path.display()))?;
                    io::copy(&mut entry, &mut BufWriter::with_capacity(1 << 20, out))
                        .with_context(|| format!("cannot extract {}", rel.display()))?;
                    restore(&path, &meta)?;
                } else {
                    let mut data = Vec::with_capacity(size as usize);
                    entry.read_to_end(&mut data).context("corrupt archive")?;
                    if !pool.write(Job { path, data, meta }) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Ok((summary, dirs, links))
}

fn safe_path(raw: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in raw.components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => bail!("refusing to extract unsafe path `{}`", raw.display()),
        }
    }
    Ok(out)
}

fn write_job(job: &Job) -> Result<()> {
    let mut out = fsutil::create_file(&job.path)
        .with_context(|| format!("cannot create {}", job.path.display()))?;
    out.write_all(&job.data)
        .with_context(|| format!("cannot write {}", job.path.display()))?;
    drop(out);
    restore(&job.path, &job.meta)
}

fn create_link(item: Deferred) -> Result<()> {
    match item {
        Deferred::Symlink(path, target, meta) => {
            if fs::symlink_metadata(&path).is_ok() {
                fs::remove_file(&path)?;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &path)
                .with_context(|| format!("cannot create symlink {}", path.display()))?;
            #[cfg(not(unix))]
            fs::write(&path, target.to_string_lossy().as_bytes())?;
            if let Some(mtime) = meta.mtime {
                let _ = fsutil::set_mtime(&path, mtime);
            }
        }
        Deferred::HardLink(path, target) => {
            if fs::symlink_metadata(&target).is_ok_and(|m| !m.is_file()) {
                bail!(
                    "refusing hard link {} to a non-regular file",
                    path.display()
                );
            }
            if fs::symlink_metadata(&path).is_ok() {
                fs::remove_file(&path)?;
            }
            fs::hard_link(&target, &path)
                .with_context(|| format!("cannot create hard link {}", path.display()))?;
        }
    }
    Ok(())
}

fn restore(path: &Path, meta: &Meta) -> Result<()> {
    if let Some(mode) = meta.mode {
        fsutil::set_mode(path, mode)
            .with_context(|| format!("cannot set permissions on {}", path.display()))?;
    }
    if let Some(mtime) = meta.mtime {
        fsutil::set_mtime(path, mtime)
            .with_context(|| format!("cannot set mtime on {}", path.display()))?;
    }
    Ok(())
}

pub fn list(archive: &Path, format: Format) -> Result<Vec<Listing>> {
    let mut tar = open(archive, format, &Progress::hidden())?;
    let mut out = Vec::new();
    for entry in tar.entries().context("cannot read archive")? {
        let entry = entry.context("corrupt archive")?;
        let header = entry.header();
        let kind = match header.entry_type() {
            EntryType::Directory => Kind::Dir,
            EntryType::Symlink | EntryType::Link => Kind::Symlink {
                target: entry
                    .link_name()?
                    .map(std::borrow::Cow::into_owned)
                    .unwrap_or_default(),
            },
            _ => Kind::File { size: entry.size() },
        };
        out.push(Listing {
            name: entry
                .path()?
                .to_string_lossy()
                .trim_end_matches('/')
                .to_owned(),
            kind,
            mtime: header.mtime().ok().map(from_unix),
        });
    }
    Ok(out)
}

fn from_unix(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("site");
        fs::create_dir_all(root.join("assets/empty")).unwrap();
        fs::write(root.join("index.html"), "<h1>hi</h1>".repeat(100_000)).unwrap();
        fs::write(root.join("assets/app.js"), "console.log(1)").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("index.html", root.join("home.html")).unwrap();
        dir
    }

    fn roundtrip(format: Format, level: u32) {
        let dir = fixture();
        let entries = walk::collect(&[dir.path().join("site")], None)
            .unwrap()
            .entries;
        let archive = dir.path().join(format!("out.{format}"));
        let file = File::create(&archive).unwrap();
        let (_, packed) = pack(&entries, file, format, level, 4, &Progress::hidden()).unwrap();
        assert_eq!(packed.files, 2);
        assert_eq!(packed.dirs, 3);

        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let unpacked = unpack(&archive, &dest, format, &Progress::hidden()).unwrap();
        assert_eq!(unpacked, packed);

        for rel in ["index.html", "assets/app.js"] {
            assert_eq!(
                fs::read(dir.path().join("site").join(rel)).unwrap(),
                fs::read(dest.join("site").join(rel)).unwrap(),
            );
        }
        assert!(dest.join("site/assets/empty").is_dir());
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(dest.join("site/home.html")).unwrap(),
            Path::new("index.html")
        );

        let listing = list(&archive, format).unwrap();
        assert_eq!(listing.len() as u64, packed.entries());
        assert_eq!(listing[0].name, "site");
    }

    #[test]
    fn roundtrips_tar() {
        roundtrip(Format::Tar, 0);
    }

    #[test]
    fn roundtrips_tar_gz() {
        roundtrip(Format::TarGz, 6);
    }

    #[test]
    fn roundtrips_tar_zst() {
        roundtrip(Format::TarZst, 3);
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.tar");
        let mut data = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut data);
            let mut header = Header::new_gnu();
            header.as_gnu_mut().unwrap().name[..13].copy_from_slice(b"../escape.txt");
            header.set_size(5);
            header.set_cksum();
            tar.append(&header, &b"pwned"[..]).unwrap();
            tar.finish().unwrap();
        }
        fs::write(&archive, data).unwrap();

        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        assert!(unpack(&archive, &dest, Format::Tar, &Progress::hidden()).is_err());
        assert!(!dir.path().join("escape.txt").exists());
    }
}
