use std::fs::{self, File};
use std::io::{self, BufWriter, IsTerminal, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::format::Format;
use crate::fsutil;
use crate::listing::Listing;
use crate::options::UnpackOptions;
use crate::progress::{Progress, Summary};
use crate::{tarball, walk, ziparc};

const WRITE_BUFFER: usize = 1 << 20;

#[derive(Debug, Clone)]
pub struct PackOptions {
    pub format: Format,
    pub level: Option<u32>,
    pub force: bool,
    pub progress: bool,
    pub verbose: bool,
    pub filters: walk::Filters,
}

#[derive(Debug)]
pub struct Packed {
    pub summary: Summary,
    pub skipped: Vec<PathBuf>,
    pub size: u64,
}

#[must_use]
pub fn is_stdio(path: &Path) -> bool {
    path.as_os_str() == "-"
}

pub fn pack(inputs: &[PathBuf], output: &Path, opts: &PackOptions) -> Result<Packed> {
    let level = opts.level.unwrap_or(opts.format.default_level());
    let levels = opts.format.levels();
    if !levels.contains(&level) {
        bail!(
            "level {level} is out of range for {} (expected {}-{})",
            opts.format,
            levels.start(),
            levels.end()
        );
    }
    let to_stdout = is_stdio(output);
    if to_stdout && io::stdout().is_terminal() {
        bail!("refusing to write an archive to a terminal (redirect stdout or pass --output FILE)");
    }
    if !to_stdout && !opts.force && fs::symlink_metadata(output).is_ok() {
        bail!(
            "{} already exists (pass --force to overwrite)",
            output.display()
        );
    }
    if let Ok(out) = fs::canonicalize(output) {
        for input in inputs {
            if fs::canonicalize(input).is_ok_and(|i| i == out) {
                bail!("{} is both an input and the output", input.display());
            }
        }
    }

    let exclude = (!to_stdout).then_some(output);
    let walk::Collected { entries, skipped } = walk::collect(inputs, exclude, &opts.filters)?;
    if entries.is_empty() {
        bail!("nothing to pack");
    }
    let progress = Progress::new(
        "packing",
        walk::total_size(&entries),
        opts.progress,
        opts.verbose,
    );

    let (summary, size) = if to_stdout {
        pack_to_stdout(&entries, opts.format, level, &progress)?
    } else {
        let tmp = fsutil::create_atomic(output)?;
        let out = BufWriter::with_capacity(WRITE_BUFFER, tmp.as_file().try_clone()?);
        let (out, summary) = pack_into(&entries, out, opts.format, level, &progress)?;
        let file =
            finish_file(out).with_context(|| format!("cannot write {}", output.display()))?;
        let size = file.metadata()?.len();
        fsutil::persist(tmp, output)?;
        (summary, size)
    };
    progress.finish();
    Ok(Packed {
        summary,
        skipped,
        size,
    })
}

fn pack_into(
    entries: &[walk::Entry],
    out: BufWriter<File>,
    format: Format,
    level: u32,
    progress: &Progress,
) -> Result<(BufWriter<File>, Summary)> {
    match format {
        Format::Zip => ziparc::pack(entries, out, level, progress),
        format => {
            let threads = rayon::current_num_threads();
            tarball::pack(entries, out, format, level, threads, progress)
        }
    }
}

fn pack_to_stdout(
    entries: &[walk::Entry],
    format: Format,
    level: u32,
    progress: &Progress,
) -> Result<(Summary, u64)> {
    let mut stdout = io::stdout();
    if format == Format::Zip {
        let out = BufWriter::with_capacity(WRITE_BUFFER, tempfile::tempfile()?);
        let (out, summary) = pack_into(entries, out, format, level, progress)?;
        let mut file = finish_file(out)?;
        file.seek(SeekFrom::Start(0))?;
        let size = io::copy(&mut file, &mut stdout)?;
        stdout.flush()?;
        return Ok((summary, size));
    }
    let counter = Counter::new(BufWriter::with_capacity(WRITE_BUFFER, stdout));
    let threads = rayon::current_num_threads();
    let (mut counter, summary) = tarball::pack(entries, counter, format, level, threads, progress)?;
    counter.flush()?;
    Ok((summary, counter.count))
}

#[derive(Debug)]
struct Counter<W> {
    inner: W,
    count: u64,
}

impl<W> Counter<W> {
    const fn new(inner: W) -> Self {
        Self { inner, count: 0 }
    }
}

impl<W: Write> Write for Counter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn finish_file(out: BufWriter<File>) -> io::Result<File> {
    let mut file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
    file.flush()?;
    Ok(file)
}

struct Input {
    path: PathBuf,
    spool: Option<tempfile::TempDir>,
}

fn materialize(archive: &Path) -> Result<Input> {
    if !is_stdio(archive) {
        return Ok(Input {
            path: archive.to_path_buf(),
            spool: None,
        });
    }
    if io::stdin().is_terminal() {
        bail!("no archive on stdin (pipe one in, for example `curl … | pakk unpack -`)");
    }
    let spool = tempfile::Builder::new().prefix(".pakk-stdin-").tempdir()?;
    fsutil::track(spool.path());
    let raw = spool.path().join("stdin");
    io::copy(&mut io::stdin().lock(), &mut File::create(&raw)?).context("cannot read stdin")?;
    let path = match Format::sniff(&mut File::open(&raw)?)? {
        Some(format) => {
            let named = spool.path().join(format!("archive.{}", format.extension()));
            fs::rename(&raw, &named)?;
            named
        }
        None => raw,
    };
    Ok(Input {
        path,
        spool: Some(spool),
    })
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Some(spool) = &self.spool {
            fsutil::untrack(spool.path());
        }
    }
}

pub fn detect(archive: &Path) -> Result<Format> {
    let mut file =
        File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    Format::sniff(&mut file)?.with_context(|| {
        format!(
            "{} is not a supported archive ({})",
            archive.display(),
            Format::names()
        )
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Display {
    pub progress: bool,
    pub verbose: bool,
}

pub fn unpack(
    archive: &Path,
    dest: Option<&Path>,
    opts: &UnpackOptions,
    display: Display,
) -> Result<(PathBuf, Summary)> {
    let input = materialize(archive)?;
    let archive = input.path.as_path();
    let format = detect(archive)?;
    let progress = Progress::new("unpacking", 0, display.progress, display.verbose);
    let result = match dest {
        Some(dest) => {
            fs::create_dir_all(dest)
                .with_context(|| format!("cannot create {}", dest.display()))?;
            extract(archive, dest, format, opts, &progress).map(|s| (dest.to_path_buf(), s))
        }
        None => unpack_here(archive, format, opts, &progress),
    };
    progress.finish();
    result
}

fn unpack_here(
    archive: &Path,
    format: Format,
    opts: &UnpackOptions,
    progress: &Progress,
) -> Result<(PathBuf, Summary)> {
    let here = Path::new(".");
    if format == Format::Zip && opts.strip == 0 {
        let tops = ziparc::top_level(archive)?;
        let target = match tops.iter().next() {
            Some(only) if tops.len() == 1 => here.join(only),
            _ => here.join(Format::stem(archive).unwrap_or_else(|| "archive".into())),
        };
        if fs::symlink_metadata(&target).is_ok() {
            bail!(
                "{} already exists (pass -C <dir> to extract elsewhere)",
                target.display()
            );
        }
    }
    let staging = tempfile::Builder::new()
        .prefix(".pakk-")
        .tempdir_in(here)
        .context("cannot create a staging directory here")?;
    fsutil::track(staging.path());
    let result = finish_staging(archive, &staging, format, opts, progress);
    fsutil::untrack(staging.path());
    result
}

fn finish_staging(
    archive: &Path,
    staging: &tempfile::TempDir,
    format: Format,
    opts: &UnpackOptions,
    progress: &Progress,
) -> Result<(PathBuf, Summary)> {
    let here = Path::new(".");
    let summary = extract(archive, staging.path(), format, opts, progress)?;

    let mut top = fs::read_dir(staging.path())?.collect::<std::io::Result<Vec<_>>>()?;
    let (from, to, whole) = if top.len() == 1 {
        let only = top.remove(0);
        (only.path(), here.join(only.file_name()), false)
    } else {
        let stem = Format::stem(archive).unwrap_or_else(|| "archive".into());
        (staging.path().to_path_buf(), here.join(stem), true)
    };
    if fs::symlink_metadata(&to).is_ok() {
        bail!(
            "{} already exists (pass -C <dir> to extract elsewhere)",
            to.display()
        );
    }
    fs::rename(&from, &to).with_context(|| format!("cannot create {}", to.display()))?;
    if whole {
        #[cfg(unix)]
        fsutil::set_mode(&to, 0o755)?;
    }
    Ok((to, summary))
}

fn extract(
    archive: &Path,
    dest: &Path,
    format: Format,
    opts: &UnpackOptions,
    progress: &Progress,
) -> Result<Summary> {
    match format {
        Format::Zip => ziparc::unpack(archive, dest, opts, progress),
        format => tarball::unpack(archive, dest, format, opts, progress),
    }
}

pub fn verify(archive: &Path, display: Display) -> Result<Summary> {
    let input = materialize(archive)?;
    let archive = input.path.as_path();
    let format = detect(archive)?;
    let progress = Progress::new("testing", 0, display.progress, display.verbose);
    let result = match format {
        Format::Zip => ziparc::verify(archive, &progress),
        format => tarball::verify(archive, format, &progress),
    };
    progress.finish();
    result
}

pub fn list(archive: &Path) -> Result<Vec<Listing>> {
    let input = materialize(archive)?;
    let archive = input.path.as_path();
    match detect(archive)? {
        Format::Zip => ziparc::list(archive),
        format => tarball::list(archive, format),
    }
}
