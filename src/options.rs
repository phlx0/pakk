use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct UnpackOptions {
    pub strip: usize,
    pub keep: bool,
    pub password: Option<String>,
    pub xattrs: bool,
}

impl UnpackOptions {
    #[must_use]
    pub fn strip(&self, path: &Path) -> Option<PathBuf> {
        let stripped: PathBuf = path.components().skip(self.strip).collect();
        (!stripped.as_os_str().is_empty()).then_some(stripped)
    }

    #[must_use]
    pub fn skips(&self, path: &Path) -> bool {
        self.keep && std::fs::symlink_metadata(path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_components() {
        let opts = UnpackOptions {
            strip: 1,
            ..UnpackOptions::default()
        };
        assert_eq!(
            opts.strip(Path::new("pkg/src/lib.rs")),
            Some(PathBuf::from("src/lib.rs"))
        );
        assert_eq!(opts.strip(Path::new("pkg")), None);
        let none = UnpackOptions::default();
        assert_eq!(none.strip(Path::new("a/b")), Some(PathBuf::from("a/b")));
    }
}
