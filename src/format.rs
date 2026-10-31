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
    TarXz,
    TarBz2,
}

const SUFFIXES: &[(&str, Format)] = &[
    (".tar.gz", Format::TarGz),
    (".tar.zst", Format::TarZst),
    (".tar.bz2", Format::TarBz2),
    (".tar.xz", Format::TarXz),
    (".tbz2", Format::TarBz2),
    (".tgz", Format::TarGz),
    (".tzst", Format::TarZst),
    (".txz", Format::TarXz),
    (".tbz", Format::TarBz2),
    (".zip", Format::Zip),
    (".tar", Format::Tar),
];

pub const SNIFF_LEN: usize = 512;

impl Format {
    pub const ALL: [Self; 6] = [
        Self::Zip,
        Self::Tar,
        Self::TarGz,
        Self::TarZst,
        Self::TarXz,
        Self::TarBz2,
    ];

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
            [0xfd, b'7', b'z', b'X', b'Z', 0, ..] => Some(Self::TarXz),
            [
                b'B',
                b'Z',
                b'h',
                b'1'..=b'9',
                0x31,
                0x41,
                0x59,
                0x26,
                0x53,
                0x59,
                ..,
            ]
            | [
                b'B',
                b'Z',
                b'h',
                b'1'..=b'9',
                0x17,
                0x72,
                0x45,
                0x38,
                0x50,
                0x90,
                ..,
            ] => Some(Self::TarBz2),
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
            Self::TarXz => "tar.xz",
            Self::TarBz2 => "tar.bz2",
        }
    }

    #[must_use]
    pub fn names() -> String {
        Self::ALL.map(Self::extension).join(", ")
    }

    #[must_use]
    pub const fn levels(self) -> RangeInclusive<u32> {
        match self {
            Self::Zip | Self::TarGz | Self::TarXz => 0..=9,
            Self::TarZst => 1..=22,
            Self::TarBz2 => 1..=9,
            Self::Tar => 0..=0,
        }
    }

    #[must_use]
    pub const fn default_level(self) -> u32 {
        match self {
            Self::Zip | Self::TarGz | Self::TarXz => 6,
            Self::TarZst => 3,
            Self::TarBz2 => 9,
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
            "tar.xz" | "txz" | "xz" => Ok(Self::TarXz),
            "tar.bz2" | "tbz2" | "tbz" | "bz2" | "bzip2" => Ok(Self::TarBz2),
            _ => Err(format!(
                "unknown format `{s}` (expected one of: {})",
                Self::names()
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
            ("a.tar.xz", Some(Format::TarXz)),
            ("a.txz", Some(Format::TarXz)),
            ("a.tar.bz2", Some(Format::TarBz2)),
            ("a.tbz2", Some(Format::TarBz2)),
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
        assert_eq!(
            Format::from_magic(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
            Some(Format::TarXz)
        );
        assert_eq!(Format::from_magic(b"BZh91AY&SY"), Some(Format::TarBz2));
        assert_eq!(Format::from_magic(b"BZh9 is just text"), None);
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
        for format in Format::ALL {
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
