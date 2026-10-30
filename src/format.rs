use std::fmt;
use std::io::{self, Read};
use std::ops::RangeInclusive;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarZst,
}

const SUFFIXES: &[(&str, Format)] = &[
    (".tar.gz", Format::TarGz),
    (".tar.zst", Format::TarZst),
    (".tgz", Format::TarGz),
    (".tzst", Format::TarZst),
    (".zip", Format::Zip),
    (".tar", Format::Tar),
];

pub const SNIFF_LEN: usize = 512;

impl Format {
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        SUFFIXES
            .iter()
            .find(|(suffix, _)| name.len() > suffix.len() && name.ends_with(suffix))
            .map(|&(_, format)| format)
    }

    #[must_use]
    pub fn from_magic(head: &[u8]) -> Option<Self> {
        match head {
            // local file header, or end-of-central-directory of an empty zip
            [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..] => Some(Self::Zip),
            [0x1f, 0x8b, ..] => Some(Self::TarGz),
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Some(Self::TarZst),
            _ if head.len() >= 262 && &head[257..262] == b"ustar" => Some(Self::Tar),
            _ => None,
        }
    }

    pub fn sniff(reader: &mut impl Read) -> io::Result<Option<Self>> {
        let mut head = Vec::with_capacity(SNIFF_LEN);
        reader.take(SNIFF_LEN as u64).read_to_end(&mut head)?;
        Ok(Self::from_magic(&head))
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarZst => "tar.zst",
        }
    }

    #[must_use]
    pub const fn levels(self) -> RangeInclusive<u32> {
        match self {
            Self::Zip | Self::TarGz => 0..=9,
            Self::TarZst => 1..=22,
            Self::Tar => 0..=0,
        }
    }

    #[must_use]
    pub const fn default_level(self) -> u32 {
        match self {
            Self::Zip | Self::TarGz => 6,
            Self::TarZst => 3,
            Self::Tar => 0,
        }
    }

    #[must_use]
    pub fn stem(path: &Path) -> Option<String> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let cut = SUFFIXES
            .iter()
            .find(|(suffix, _)| lower.len() > suffix.len() && lower.ends_with(suffix))
            .map_or(name.len(), |(suffix, _)| name.len() - suffix.len());
        Some(name[..cut].to_owned())
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "zip" => Ok(Self::Zip),
            "tar" => Ok(Self::Tar),
            "tar.gz" | "tgz" | "gz" | "gzip" => Ok(Self::TarGz),
            "tar.zst" | "tzst" | "zst" | "zstd" => Ok(Self::TarZst),
            _ => Err(format!(
                "unknown format `{s}` (expected zip, tar, tar.gz or tar.zst)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_format_from_name() {
        let cases = [
            ("a.zip", Some(Format::Zip)),
            ("A.ZIP", Some(Format::Zip)),
            ("a.tar", Some(Format::Tar)),
            ("a.tar.gz", Some(Format::TarGz)),
            ("a.tgz", Some(Format::TarGz)),
            ("dir/a.tar.zst", Some(Format::TarZst)),
            ("a.tzst", Some(Format::TarZst)),
            ("a.gz", None),
            ("a.txt", None),
            (".zip", None),
        ];
        for (name, expected) in cases {
            assert_eq!(Format::from_path(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn detects_format_from_magic() {
        assert_eq!(Format::from_magic(b"PK\x03\x04rest"), Some(Format::Zip));
        assert_eq!(Format::from_magic(b"PK\x05\x06"), Some(Format::Zip));
        assert_eq!(Format::from_magic(&[0x1f, 0x8b, 8]), Some(Format::TarGz));
        assert_eq!(
            Format::from_magic(&[0x28, 0xb5, 0x2f, 0xfd]),
            Some(Format::TarZst)
        );
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(Format::from_magic(&tar), Some(Format::Tar));
        assert_eq!(Format::from_magic(b"hello"), None);
        assert_eq!(Format::from_magic(b""), None);
    }

    #[test]
    fn parses_format_names() {
        assert_eq!("zip".parse(), Ok(Format::Zip));
        assert_eq!(".TGZ".parse(), Ok(Format::TarGz));
        assert_eq!("zstd".parse(), Ok(Format::TarZst));
        assert!("rar".parse::<Format>().is_err());
    }

    #[test]
    fn default_level_is_in_range() {
        for format in [Format::Zip, Format::Tar, Format::TarGz, Format::TarZst] {
            assert!(
                format.levels().contains(&format.default_level()),
                "{format}"
            );
        }
    }

    #[test]
    fn strips_archive_suffix() {
        assert_eq!(
            Format::stem(Path::new("x/site.tar.gz")).as_deref(),
            Some("site")
        );
        assert_eq!(
            Format::stem(Path::new("Photos.ZIP")).as_deref(),
            Some("Photos")
        );
        assert_eq!(
            Format::stem(Path::new("notes.txt")).as_deref(),
            Some("notes.txt")
        );
    }
}
