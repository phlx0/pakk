use std::io::{Read, Write};

use indicatif::{ProgressBar, ProgressBarIter, ProgressDrawTarget, ProgressStyle};

#[derive(Debug, Clone)]
pub struct Progress {
    bar: ProgressBar,
    verbose: bool,
}

impl Progress {
    #[must_use]
    pub fn new(verb: &str, total: u64, visible: bool, verbose: bool) -> Self {
        if !visible {
            return Self {
                verbose,
                ..Self::hidden()
            };
        }
        let bar = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
        bar.set_style(
            ProgressStyle::with_template(
                "{prefix:>10.cyan.bold} {bar:32.cyan/dim} {binary_bytes:>10} / {binary_total_bytes:<10} {binary_bytes_per_sec:>12}  {eta}",
            )
            .expect("valid template")
            .progress_chars("━╸─"),
        );
        bar.set_prefix(verb.to_owned());
        Self { bar, verbose }
    }

    #[must_use]
    pub fn hidden() -> Self {
        Self {
            bar: ProgressBar::hidden(),
            verbose: false,
        }
    }

    pub fn entry(&self, name: &str) {
        if !self.verbose {
            return;
        }
        #[cfg(windows)]
        let name = &name.replace('\\', "/");
        if self.bar.is_hidden() {
            eprintln!("{name}");
        } else {
            self.bar.println(name);
        }
    }

    pub fn inc(&self, bytes: u64) {
        self.bar.inc(bytes);
    }

    pub fn set_total(&self, total: u64) {
        self.bar.set_length(total);
    }

    pub fn wrap_read<R: Read>(&self, reader: R) -> ProgressBarIter<R> {
        self.bar.wrap_read(reader)
    }

    pub fn wrap_write<W: Write>(&self, writer: W) -> ProgressBarIter<W> {
        self.bar.wrap_write(writer)
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
}

impl Summary {
    #[must_use]
    pub const fn entries(&self) -> u64 {
        self.files + self.dirs + self.symlinks
    }
}
