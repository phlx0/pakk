use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use pakk::format::Format;

#[derive(Debug, Parser)]
#[command(
    name = "pakk",
    version,
    about = "Create and extract zip, tar.gz and tar.zst archives using every core you have",
    after_help = "Examples:\n  pakk pack photos/              → photos.zip\n  pakk pack src/ -f tar.zst      → src.tar.zst\n  pakk unpack release.tar.gz     → ./release/\n  pakk list backup.zip -l"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Number of worker threads (default: all cores)
    #[arg(short = 'j', long, global = true, value_name = "N")]
    pub threads: Option<usize>,

    /// Suppress progress bars and summaries
    #[arg(short, long, global = true)]
    pub quiet: bool,
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

    /// Archive to create; its extension picks the format
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Archive format when it cannot be inferred from --output [zip, tar, tar.gz, tar.zst]
    #[arg(short, long, value_name = "FORMAT")]
    pub format: Option<Format>,

    /// Compression level: 0-9 for zip and tar.gz, 1-22 for tar.zst
    #[arg(short, long, value_name = "N")]
    pub level: Option<u32>,

    /// Overwrite the output if it already exists
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct UnpackArgs {
    /// Archive to extract (format is detected from its contents)
    pub archive: PathBuf,

    /// Extract into this directory instead of next to the archive's name
    #[arg(short = 'C', long = "dir", value_name = "DIR")]
    pub dest: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Archive to inspect
    pub archive: PathBuf,

    /// Show sizes and modification times
    #[arg(short, long)]
    pub long: bool,
}
