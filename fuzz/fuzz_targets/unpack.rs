#![no_main]

use std::fs;

use libfuzzer_sys::fuzz_target;
use pakk::format::Format;
use pakk::options::UnpackOptions;
use pakk::progress::Progress;
use pakk::{tarball, ziparc};

fuzz_target!(|data: &[u8]| {
    let Some(format) = Format::from_magic(data) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("input");
    let dest = dir.path().join("dest");
    fs::write(&archive, data).unwrap();
    fs::create_dir(&dest).unwrap();
    let _ = match format {
        Format::Zip => ziparc::unpack(&archive, &dest, &UnpackOptions::default(), &Progress::hidden()),
        format => {
            tarball::unpack(&archive, &dest, format, &UnpackOptions::default(), &Progress::hidden())
        }
    };
    let mut siblings: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    siblings.sort();
    assert_eq!(siblings, ["dest", "input"], "extraction escaped its destination");
});
