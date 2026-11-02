mod cli;

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
use console::style;
use indicatif::{HumanBytes, HumanCount};
use pakk::format::Format;
use pakk::listing::Listing;
use pakk::ops::{self, PackOptions};
use pakk::options::UnpackOptions;
use pakk::progress::Summary;
use pakk::walk::Kind;

use crate::cli::{Cli, Command, ListArgs, PackArgs, UnpackArgs};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let _ = ctrlc::set_handler(|| {
        pakk::fsutil::remove_tracked();
        std::process::exit(130);
    });
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if is_broken_pipe(&err) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{} {err:#}", style("error:").red().bold());
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Some(threads) = cli.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .context("cannot start thread pool")?;
    }
    let show = !cli.quiet && io::stderr().is_terminal();
    match cli.command {
        Command::Pack(args) => pack(&args, show, cli.verbose, cli.quiet),
        Command::Unpack(args) => unpack(&args, show, cli.verbose, cli.quiet),
        Command::List(args) => list(&args),
        Command::Test(args) => {
            let started = Instant::now();
            let archive = args.archive;
            let opts = UnpackOptions {
                password: password(&archive, args.password)?,
                ..UnpackOptions::default()
            };
            let display = ops::Display {
                progress: show,
                verbose: cli.verbose,
            };
            let summary = ops::verify(&archive, &opts, display)?;
            if !cli.quiet {
                report(
                    "verified",
                    &summary,
                    &format!("in {}, all checksums match,", archive.display()),
                    started,
                );
            }
            Ok(())
        }
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "pakk", &mut io::stdout());
            Ok(())
        }
    }
}

fn pack(args: &PackArgs, progress: bool, verbose: bool, quiet: bool) -> Result<()> {
    let (output, format) = resolve_output(args)?;
    let started = Instant::now();
    let opts = PackOptions {
        format,
        level: args.level,
        force: args.force,
        progress,
        verbose,
        filters: pakk::walk::Filters {
            exclude: args.exclude.clone(),
            gitignore: args.gitignore,
        },
    };
    let ops::Packed {
        summary,
        skipped,
        size,
    } = ops::pack(&args.inputs, &output, &opts)?;
    for path in &skipped {
        eprintln!(
            "{} skipped {} (not a regular file, directory or symlink)",
            style("warning:").yellow().bold(),
            path.display()
        );
    }

    if !quiet {
        let ratio = if summary.bytes > 0 {
            format!(", {:.0}%", size as f64 * 100.0 / summary.bytes as f64)
        } else {
            String::new()
        };
        report(
            "packed",
            &summary,
            &format!(
                "into {} ({}{ratio})",
                display_output(&output),
                HumanBytes(size)
            ),
            started,
        );
    }
    Ok(())
}

fn display_output(path: &Path) -> String {
    if ops::is_stdio(path) {
        "stdout".into()
    } else {
        path.display().to_string()
    }
}

fn resolve_output(args: &PackArgs) -> Result<(PathBuf, Format)> {
    if let Some(output) = &args.output {
        let format = args
            .format
            .or_else(|| Format::from_path(output))
            .with_context(|| {
                format!(
                    "cannot infer the format of {}; pass --format",
                    output.display()
                )
            })?;
        return Ok((output.clone(), format));
    }
    let format = args.format.unwrap_or(Format::Zip);
    let base = match args.inputs.as_slice() {
        [single] => std::fs::canonicalize(single)
            .with_context(|| format!("cannot read {}", single.display()))?
            .file_name()
            .map(ToOwned::to_owned),
        _ => None,
    };
    let Some(base) = base else {
        if args.inputs.len() > 1 {
            bail!("pass --output when packing several inputs");
        }
        bail!(
            "cannot derive an archive name from {}; pass --output",
            args.inputs[0].display()
        );
    };
    let mut name = base;
    name.push(".");
    name.push(format.extension());
    Ok((PathBuf::from(name), format))
}

fn unpack(args: &UnpackArgs, progress: bool, verbose: bool, quiet: bool) -> Result<()> {
    let started = Instant::now();
    let opts = UnpackOptions {
        strip: args.strip,
        keep: args.keep,
        password: password(&args.archive, args.password.clone())?,
    };
    let display = ops::Display { progress, verbose };
    let (dest, summary) = ops::unpack(&args.archive, args.dest.as_deref(), &opts, display)?;
    if !quiet {
        report(
            "unpacked",
            &summary,
            &format!("into {}", display_dir(&dest)),
            started,
        );
    }
    Ok(())
}

fn password(archive: &Path, given: Option<String>) -> Result<Option<String>> {
    if given.is_some() || !io::stdin().is_terminal() || !ops::needs_password(archive)? {
        return Ok(given);
    }
    let entered = rpassword::prompt_password(format!("password for {}: ", archive.display()))
        .context("cannot read password")?;
    Ok(Some(entered))
}

fn display_dir(path: &Path) -> String {
    let shown = path.strip_prefix(".").unwrap_or(path);
    if shown.is_relative() {
        format!("./{}", shown.display())
    } else {
        shown.display().to_string()
    }
}

fn report(verb: &str, summary: &Summary, target: &str, started: Instant) {
    let files = summary.files;
    eprintln!(
        "{} {verb} {} {} ({}) {target} in {:.2}s",
        style("✓").green().bold(),
        HumanCount(files),
        if files == 1 { "file" } else { "files" },
        HumanBytes(summary.bytes),
        started.elapsed().as_secs_f64(),
    );
}

fn list(args: &ListArgs) -> Result<()> {
    let entries = ops::list(&args.archive)?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    for entry in &entries {
        if args.long {
            write!(
                out,
                "{:>10}  {:<16}  ",
                size_column(entry),
                time_column(entry.mtime)
            )?;
        }
        writeln!(out, "{}", name_column(entry))?;
    }
    out.flush()?;
    Ok(())
}

fn size_column(entry: &Listing) -> String {
    match entry.kind {
        Kind::File { size } => HumanBytes(size).to_string(),
        _ => "-".into(),
    }
}

fn time_column(mtime: Option<SystemTime>) -> String {
    mtime
        .and_then(|t| jiff::Timestamp::try_from(t).ok())
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn name_column(entry: &Listing) -> String {
    match &entry.kind {
        Kind::Dir => format!("{}/", style(&entry.name).blue().bold()),
        Kind::Symlink { target } if !target.as_os_str().is_empty() => {
            format!("{} -> {}", style(&entry.name).cyan(), target.display())
        }
        Kind::Symlink { .. } => style(&entry.name).cyan().to_string(),
        Kind::File { .. } => entry.name.clone(),
    }
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<io::Error>())
        .any(|e| e.kind() == io::ErrorKind::BrokenPipe)
}
