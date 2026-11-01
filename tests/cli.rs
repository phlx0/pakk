use std::fs;
use std::path::Path;
use std::process::Command;

use assert_cmd::Command as Bin;
use predicates::prelude::*;
use tempfile::TempDir;

fn pakk(dir: &Path) -> Bin {
    let mut cmd = Bin::cargo_bin("pakk").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(root.join("src/empty")).unwrap();
    fs::write(root.join("README.md"), "# project\n".repeat(1000)).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    dir
}

fn assert_same_tree(a: &Path, b: &Path) {
    for entry in walkdir(a) {
        let rel = entry.strip_prefix(a).unwrap();
        let other = b.join(rel);
        if entry.is_dir() {
            assert!(other.is_dir(), "missing dir {}", rel.display());
        } else {
            assert_eq!(
                fs::read(&entry).unwrap(),
                fs::read(&other).unwrap(),
                "{}",
                rel.display()
            );
        }
    }
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walkdir(&path));
        }
        out.push(path);
    }
    out
}

fn has(tool: &str) -> bool {
    Command::new(tool).arg("--help").output().is_ok()
}

#[test]
fn roundtrips_every_format() {
    for format in ["zip", "tar", "tar.gz", "tar.zst", "tar.xz", "tar.bz2"] {
        let ws = workspace();
        pakk(ws.path())
            .args(["pack", "project", "-f", format])
            .assert()
            .success();
        let archive = format!("project.{format}");
        assert!(ws.path().join(&archive).is_file(), "{archive}");

        pakk(ws.path())
            .args(["unpack", &archive, "-C", "out"])
            .assert()
            .success();
        assert_same_tree(&ws.path().join("project"), &ws.path().join("out/project"));
    }
}

#[test]
fn unpacks_single_root_next_to_archive() {
    let ws = workspace();
    pakk(ws.path())
        .args(["pack", "project", "-o", "p.tar.zst"])
        .assert()
        .success();
    fs::rename(ws.path().join("project"), ws.path().join("original")).unwrap();

    pakk(ws.path())
        .args(["unpack", "p.tar.zst"])
        .assert()
        .success()
        .stderr(predicate::str::contains("into ./project"));
    assert_same_tree(&ws.path().join("original"), &ws.path().join("project"));
    let leftovers: Vec<_> = fs::read_dir(ws.path())
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().into_string().ok())
        .filter(|n| n.starts_with(".pakk-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn never_spills_multiple_roots_into_cwd() {
    let ws = workspace();
    pakk(ws.path())
        .args(["pack", "project/.", "-o", "bomb.zip"])
        .assert()
        .success();
    pakk(ws.path())
        .args(["unpack", "bomb.zip"])
        .assert()
        .success();
    assert!(ws.path().join("bomb/README.md").is_file());
    assert!(ws.path().join("bomb/src/main.rs").is_file());
    assert!(!ws.path().join("README.md").exists());
}

#[test]
fn refuses_to_clobber() {
    let ws = workspace();
    pakk(ws.path()).args(["pack", "project"]).assert().success();
    pakk(ws.path())
        .args(["pack", "project"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    pakk(ws.path())
        .args(["pack", "project", "--force"])
        .assert()
        .success();
    pakk(ws.path())
        .args(["unpack", "project.zip"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    let staging = fs::read_dir(ws.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pakk-")
        })
        .count();
    assert_eq!(staging, 0);
}

#[cfg(unix)]
#[test]
fn explains_unnamable_input() {
    let ws = workspace();
    pakk(ws.path())
        .args(["pack", "/"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot derive an archive name"));
}

#[test]
fn detects_format_from_content() {
    let ws = workspace();
    pakk(ws.path())
        .args(["pack", "project", "-o", "p.tar.gz"])
        .assert()
        .success();
    fs::rename(ws.path().join("p.tar.gz"), ws.path().join("mystery.bin")).unwrap();
    pakk(ws.path())
        .args(["list", "mystery.bin"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("project/README.md")
                .and(predicate::str::contains("project/src/")),
        );
}

#[test]
fn lists_in_long_form() {
    let ws = workspace();
    pakk(ws.path()).args(["pack", "project"]).assert().success();
    pakk(ws.path())
        .args(["ls", "-l", "project.zip"])
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(r"9\.77 KiB\s+\d{4}-\d\d-\d\d \d\d:\d\d\s+project/README\.md")
                .unwrap(),
        );
}

#[test]
fn quiet_is_silent() {
    let ws = workspace();
    pakk(ws.path())
        .args(["-q", "pack", "project"])
        .assert()
        .success()
        .stderr("")
        .stdout("");
}

#[test]
fn reports_bad_input() {
    let ws = workspace();
    pakk(ws.path())
        .args(["unpack", "project/README.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a supported archive"));
    pakk(ws.path())
        .args(["pack", "project", "-f", "tar.zst", "-l", "40"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("out of range"));
    pakk(ws.path())
        .args(["pack", "project", "-o", "out.rar"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--format"));
    assert!(!ws.path().join("out.rar").exists());
}

#[test]
fn prints_completions() {
    let ws = workspace();
    pakk(ws.path())
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef pakk"));
}

#[test]
fn system_tar_reads_our_tar_gz() {
    if !has("tar") {
        return;
    }
    let ws = workspace();
    pakk(ws.path())
        .args(["pack", "project", "-f", "tar.gz"])
        .assert()
        .success();
    fs::create_dir(ws.path().join("sys")).unwrap();
    let status = Command::new("tar")
        .args(["xzf", "../project.tar.gz"])
        .current_dir(ws.path().join("sys"))
        .status()
        .unwrap();
    assert!(status.success());
    assert_same_tree(&ws.path().join("project"), &ws.path().join("sys/project"));
}

#[test]
fn system_tar_reads_our_xz_and_bz2() {
    if !has("tar") {
        return;
    }
    for (format, flag) in [("tar.xz", "xJf"), ("tar.bz2", "xjf")] {
        let ws = workspace();
        pakk(ws.path())
            .args(["pack", "project", "-f", format])
            .assert()
            .success();
        fs::create_dir(ws.path().join("sys")).unwrap();
        let status = Command::new("tar")
            .args([flag, &format!("../project.{format}")])
            .current_dir(ws.path().join("sys"))
            .status()
            .unwrap();
        assert!(status.success(), "{format}");
        assert_same_tree(&ws.path().join("project"), &ws.path().join("sys/project"));
    }
}

#[test]
fn system_unzip_reads_our_zip() {
    if !has("unzip") {
        return;
    }
    let ws = workspace();
    pakk(ws.path()).args(["pack", "project"]).assert().success();
    let status = Command::new("unzip")
        .args(["-q", "project.zip", "-d", "sys"])
        .current_dir(ws.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_same_tree(&ws.path().join("project"), &ws.path().join("sys/project"));
}

#[test]
fn refuses_to_pack_a_file_onto_itself() {
    let ws = workspace();
    let notes = ws.path().join("project/README.md");
    let before = fs::read(&notes).unwrap();
    pakk(ws.path())
        .args([
            "pack",
            "project/README.md",
            "-o",
            "project/README.md",
            "-f",
            "zip",
            "--force",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("both an input and the output"));
    assert_eq!(fs::read(&notes).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn warns_about_special_files_instead_of_hanging() {
    let ws = workspace();
    let fifo = ws.path().join("project/pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    pakk(ws.path())
        .args(["pack", "project"])
        .timeout(std::time::Duration::from_secs(20))
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped").and(predicate::str::contains("pipe")));
}

#[cfg(unix)]
#[test]
fn reextracts_over_read_only_files() {
    use std::os::unix::fs::PermissionsExt;
    let ws = workspace();
    let ro = ws.path().join("project/src/main.rs");
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o444)).unwrap();
    for format in ["zip", "tar.gz"] {
        pakk(ws.path())
            .args(["pack", "project", "-f", format, "--force"])
            .assert()
            .success();
        let archive = format!("project.{format}");
        for _ in 0..2 {
            pakk(ws.path())
                .args(["unpack", &archive, "-C", "again"])
                .assert()
                .success();
        }
    }
}

#[cfg(unix)]
#[test]
fn never_extracts_through_existing_symlinks() {
    let ws = workspace();
    pakk(ws.path())
        .args(["pack", "project", "-f", "tar.gz"])
        .assert()
        .success();
    pakk(ws.path()).args(["pack", "project"]).assert().success();
    fs::create_dir_all(ws.path().join("outside")).unwrap();
    for archive in ["project.zip", "project.tar.gz"] {
        let dest = ws.path().join(format!("dest-{archive}"));
        fs::create_dir(&dest).unwrap();
        std::os::unix::fs::symlink("../outside", dest.join("project")).unwrap();
        pakk(ws.path())
            .args(["unpack", archive, "-C"])
            .arg(&dest)
            .assert()
            .failure()
            .stderr(predicate::str::contains("existing symlink"));
    }
    assert_eq!(fs::read_dir(ws.path().join("outside")).unwrap().count(), 0);
}

#[test]
fn last_duplicate_entry_wins() {
    let ws = workspace();
    let mut data = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut data);
        for i in 0..400 {
            let body = vec![if i % 2 == 0 { b'A' } else { b'B' }; 1000 + i];
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "f", body.as_slice()).unwrap();
        }
        tar.finish().unwrap();
    }
    fs::write(ws.path().join("dup.tar"), data).unwrap();
    for run in 0..10 {
        let dest = format!("out{run}");
        pakk(ws.path())
            .args(["unpack", "dup.tar", "-C", &dest])
            .assert()
            .success();
        assert_eq!(
            fs::read(ws.path().join(dest).join("f")).unwrap(),
            vec![b'B'; 1399]
        );
    }
}

#[cfg(unix)]
#[test]
fn interrupt_removes_temporary_files() {
    let ws = workspace();
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let noise: Vec<u8> = (0..(192 << 20))
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect();
    fs::write(ws.path().join("project/noise.bin"), noise).unwrap();

    let leftovers = || {
        fs::read_dir(ws.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".pakk-")
            })
            .count()
    };
    let bin = assert_cmd::cargo::cargo_bin("pakk");
    let mut child = Command::new(bin)
        .args([
            "-q", "-j", "1", "pack", "project", "-o", "slow.zip", "-l", "9",
        ])
        .current_dir(ws.path())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while leftovers() == 0 && started.elapsed().as_secs() < 20 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(leftovers(), 1, "temp file should exist while packing");
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(leftovers(), 0);
    assert!(!ws.path().join("slow.zip").exists());
}

#[test]
fn extracts_every_common_zip_method() {
    if Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let ws = workspace();
    let script = r#"
import zipfile
for name, method in [("bzip2", zipfile.ZIP_BZIP2), ("lzma", zipfile.ZIP_LZMA), ("deflate", zipfile.ZIP_DEFLATED)]:
    with zipfile.ZipFile(f"{name}.zip", "w", method) as z:
        z.writestr("data/hello.txt", "hello " * 1000)
"#;
    assert!(
        Command::new("python3")
            .args(["-c", script])
            .current_dir(ws.path())
            .status()
            .unwrap()
            .success()
    );
    for name in ["bzip2", "lzma", "deflate"] {
        let dest = format!("out-{name}");
        pakk(ws.path())
            .args(["unpack", &format!("{name}.zip"), "-C", &dest])
            .assert()
            .success();
        assert_eq!(
            fs::read_to_string(ws.path().join(dest).join("data/hello.txt")).unwrap(),
            "hello ".repeat(1000)
        );
    }
}

#[test]
fn decompresses_single_files() {
    let ws = workspace();
    let text = "line of a log file\n".repeat(10_000);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, text.as_bytes()).unwrap();
    fs::write(ws.path().join("app.log.gz"), gz.finish().unwrap()).unwrap();
    fs::write(
        ws.path().join("app2.log.zst"),
        zstd::encode_all(text.as_bytes(), 3).unwrap(),
    )
    .unwrap();

    pakk(ws.path())
        .args(["unpack", "app.log.gz"])
        .assert()
        .success()
        .stderr(predicate::str::contains("into ./app.log"));
    assert_eq!(fs::read_to_string(ws.path().join("app.log")).unwrap(), text);

    pakk(ws.path())
        .args(["unpack", "app2.log.zst", "-C", "out"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(ws.path().join("out/app2.log")).unwrap(),
        text
    );

    pakk(ws.path())
        .args(["list", "-l", "app.log.gz"])
        .assert()
        .success()
        .stdout(predicate::str::contains("app.log"));
}

#[test]
fn excludes_and_respects_gitignore() {
    let ws = workspace();
    fs::create_dir_all(ws.path().join("project/target/debug")).unwrap();
    fs::write(ws.path().join("project/target/debug/app"), "binary").unwrap();
    fs::write(ws.path().join("project/.gitignore"), "target/\n").unwrap();
    fs::write(ws.path().join("project/notes.tmp"), "scratch").unwrap();

    pakk(ws.path())
        .args([
            "pack",
            "project",
            "-o",
            "a.zip",
            "--gitignore",
            "-x",
            "*.tmp",
        ])
        .assert()
        .success();
    pakk(ws.path())
        .args(["list", "a.zip"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("project/src/main.rs")
                .and(predicate::str::contains("target").not())
                .and(predicate::str::contains("notes.tmp").not()),
        );
}
