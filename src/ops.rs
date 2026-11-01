use std::fs::{self, File};
use std::io::{BufWriter, Write};
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
    if !opts.force && fs::symlink_metadata(output).is_ok() {
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
    let walk::Collected { entries, skipped } = walk::collect(inputs, Some(output), &opts.filters)?;
    if entries.is_empty() {
        bail!("nothing to pack");
    }
    let progress = Progress::new(
        "packing",
        walk::total_size(&entries),
        opts.progress,
        opts.verbose,
    );
    let tmp = fsutil::create_atomic(output)?;
    let out = BufWriter::with_capacity(WRITE_BUFFER, tmp.as_file().try_clone()?);

    let (out, summary) = match opts.format {
        Format::Zip => ziparc::pack(&entries, out, level, &progress)?,
        format => {
            let threads = rayon::current_num_threads();
            tarball::pack(&entries, out, format, level, threads, &progress)?
        }
    };
    finish_file(out).with_context(|| format!("cannot write {}", output.display()))?;
    progress.finish();
    fsutil::persist(tmp, output)?;
    Ok(Packed { summary, skipped })
}

fn finish_file(out: BufWriter<File>) -> std::io::Result<()> {
    let mut file = out
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.flush()
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

pub fn list(archive: &Path) -> Result<Vec<Listing>> {
    match detect(archive)? {
        Format::Zip => ziparc::list(archive),
        format => tarball::list(archive, format),
    }
}
