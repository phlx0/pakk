use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use tar::{EntryType, Header, HeaderMode};

use crate::format::Format;
use crate::fsutil;
use crate::listing::Listing;
use crate::pgzip::{self, ParGzWriter};
use crate::progress::{Progress, Summary};
use crate::walk::{Entry, Kind};

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

pub fn pack<W: Write + Send + 'static>(
    entries: &[Entry],
    out: W,
    format: Format,
    level: u32,
    threads: usize,
    progress: &Progress,
) -> Result<(W, Summary)> {
    let mut tar = tar::Builder::new(Encoder::new(out, format, level, threads)?);
    tar.mode(HeaderMode::Complete);
    tar.follow_symlinks(false);

    let mut summary = Summary::default();
    for entry in entries {
        let context = || format!("cannot add {}", entry.path.display());
        match entry.kind {
            Kind::File { .. } => {
                let file = File::open(&entry.path).with_context(context)?;
                let meta = file.metadata().with_context(context)?;
                let mut header = Header::new_gnu();
                header.set_metadata_in_mode(&meta, HeaderMode::Complete);
                tar.append_data(&mut header, &entry.name, progress.wrap_read(file))
                    .with_context(context)?;
                summary.files += 1;
                summary.bytes += meta.len();
            }
            Kind::Dir => {
                tar.append_path_with_name(&entry.path, &entry.name)
                    .with_context(context)?;
                summary.dirs += 1;
            }
            Kind::Symlink { .. } => {
                tar.append_path_with_name(&entry.path, &entry.name)
                    .with_context(context)?;
                summary.symlinks += 1;
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

pub fn unpack(archive: &Path, dest: &Path, format: Format, progress: &Progress) -> Result<Summary> {
    let mut tar = open(archive, format, progress)?;
    tar.set_preserve_permissions(true);
    tar.set_preserve_mtime(true);
    tar.set_overwrite(true);

    let mut summary = Summary::default();
    let mut dir_times: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in tar.entries().context("cannot read archive")? {
        let mut entry = entry.context("corrupt archive")?;
        let path = entry.path()?.into_owned();
        let kind = entry.header().entry_type();
        if !entry
            .unpack_in(dest)
            .with_context(|| format!("cannot extract {}", path.display()))?
        {
            bail!("refusing to extract unsafe path `{}`", path.display());
        }
        match kind {
            EntryType::Directory => {
                summary.dirs += 1;
                if let Ok(mtime) = entry.header().mtime() {
                    dir_times.push((dest.join(&path), from_unix(mtime)));
                }
            }
            EntryType::Symlink | EntryType::Link => summary.symlinks += 1,
            _ => {
                summary.files += 1;
                summary.bytes += entry.size();
            }
        }
    }
    for (dir, mtime) in dir_times.iter().rev() {
        let _ = fsutil::set_mtime(dir, *mtime);
    }
    Ok(summary)
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
        let entries = walk::collect(&[dir.path().join("site")], None).unwrap();
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
