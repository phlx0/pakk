use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::format::Format;
use crate::fsutil;
use crate::listing::Listing;
use crate::progress::{Progress, Summary};
use crate::{tarball, walk, ziparc};

const WRITE_BUFFER: usize = 1 << 20;

#[derive(Debug, Clone)]
pub struct PackOptions {
    pub format: Format,
    pub level: Option<u32>,
    pub force: bool,
    pub progress: bool,
}

pub fn pack(inputs: &[PathBuf], output: &Path, opts: &PackOptions) -> Result<Summary> {
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

    let entries = walk::collect(inputs, Some(output))?;
    let progress = Progress::new("packing", walk::total_size(&entries), opts.progress);
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
    Ok(summary)
}

fn finish_file(out: BufWriter<File>) -> std::io::Result<()> {
    let mut file = out
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.flush()?;
    file.sync_all()
}

pub fn detect(archive: &Path) -> Result<Format> {
    let mut file =
        File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    Format::sniff(&mut file)?.with_context(|| {
        format!(
            "{} is not a zip, tar, tar.gz or tar.zst archive",
            archive.display()
        )
    })
}

pub fn unpack(archive: &Path, dest: Option<&Path>, progress: bool) -> Result<(PathBuf, Summary)> {
    let format = detect(archive)?;
    let progress = Progress::new("unpacking", 0, progress);
    let result = match dest {
        Some(dest) => {
            fs::create_dir_all(dest)
                .with_context(|| format!("cannot create {}", dest.display()))?;
            extract(archive, dest, format, &progress).map(|s| (dest.to_path_buf(), s))
        }
        None => unpack_here(archive, format, &progress),
    };
    progress.finish();
    result
}

fn unpack_here(archive: &Path, format: Format, progress: &Progress) -> Result<(PathBuf, Summary)> {
    let here = Path::new(".");
    let staging = tempfile::Builder::new()
        .prefix(".pakk-")
        .tempdir_in(here)
        .context("cannot create a staging directory here")?;
    let summary = extract(archive, staging.path(), format, progress)?;

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
        let _ = staging.keep();
        #[cfg(unix)]
        fsutil::set_mode(&to, 0o755)?;
    }
    Ok((to, summary))
}

fn extract(archive: &Path, dest: &Path, format: Format, progress: &Progress) -> Result<Summary> {
    match format {
        Format::Zip => ziparc::unpack(archive, dest, progress),
        format => tarball::unpack(archive, dest, format, progress),
    }
}

pub fn list(archive: &Path) -> Result<Vec<Listing>> {
    match detect(archive)? {
        Format::Zip => ziparc::list(archive),
        format => tarball::list(archive, format),
    }
}
