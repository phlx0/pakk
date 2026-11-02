use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use pakk::format::Format;

#[derive(Debug, Parser)]
#[command(
    name = "pakk",
    version,
    about = "Create and extract zip, tar.gz, tar.zst, tar.xz and tar.bz2 archives using every core you have",
    after_help = "Examples:\n  pakk pack photos/              → photos.zip\n  pakk pack src/ -f tar.zst      → src.tar.zst\n  pakk unpack release.tar.gz     → ./release/\n  pakk list backup.zip -l"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Number of worker threads (default: all cores)
    #[arg(short = 'j', long, global = true, value_name = "N")]
    pub threads: Option<usize>,

    /// Suppress progress bars and summaries
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Print every entry as it is processed
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create an archive from files and directories
    #[command(visible_aliases = ["p", "c"])]
    Pack(PackArgs),
    /// Extract an archive, never spilling files into the current directory
    #[command(visible_aliases = ["x", "u"])]
    Unpack(UnpackArgs),
    /// List the contents of an archive
    #[command(visible_aliases = ["l", "ls"])]
    List(ListArgs),
    /// Check an archive's integrity without extracting it
    #[command(visible_alias = "t")]
    Test(TestArgs),
    /// Print shell completions
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Args)]
pub struct PackArgs {
    /// Files and directories to add; a trailing `/.` adds only the contents
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,

    /// Archive to create, or `-` for stdout; its extension picks the format
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Archive format when it cannot be inferred from --output [zip, tar, tar.gz, tar.zst, tar.xz, tar.bz2]
    #[arg(short, long, value_name = "FORMAT")]
    pub format: Option<Format>,

    /// Compression level: 0-9 for zip, tar.gz and tar.xz, 1-9 for tar.bz2, 1-22 for tar.zst
    #[arg(short, long, value_name = "N")]
    pub level: Option<u32>,

    /// Overwrite the output if it already exists
    #[arg(long)]
    pub force: bool,

    /// Leave out paths matching a gitignore-style glob (repeatable)
    #[arg(short = 'x', long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Leave out files ignored by .gitignore, .ignore and .git/info/exclude
    #[arg(short, long)]
    pub gitignore: bool,
}

#[derive(Debug, Args)]
pub struct UnpackArgs {
    /// Archive to extract, or `-` for stdin (format is detected from its contents)
    pub archive: PathBuf,

    /// Extract into this directory instead of next to the archive's name
    #[arg(short = 'C', long = "dir", value_name = "DIR")]
    pub dest: Option<PathBuf>,

    /// Drop this many leading path components from every entry
    #[arg(
        long = "strip-components",
        visible_alias = "strip",
        value_name = "N",
        default_value_t = 0
    )]
    pub strip: usize,

    /// Never overwrite existing files
    #[arg(short, long)]
    pub keep: bool,

    /// Password for encrypted zip entries (prompted for when needed and omitted)
    #[arg(short = 'P', long, env = "PAKK_PASSWORD", hide_env_values = true)]
    pub password: Option<String>,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// Archive to verify, or `-` for stdin
    pub archive: PathBuf,

    /// Password for encrypted zip entries (prompted for when needed and omitted)
    #[arg(short = 'P', long, env = "PAKK_PASSWORD", hide_env_values = true)]
    pub password: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Archive to inspect
    pub archive: PathBuf,

    /// Show sizes and modification times
    #[arg(short, long)]
    pub long: bool,
}
