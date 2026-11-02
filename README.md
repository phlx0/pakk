# pakk

**Create and extract zip, tar.gz, tar.zst, tar.xz and tar.bz2 archives using every core you have.**

`zip`, `unzip`, `tar` and `gzip` were designed when computers had one core. Your laptop has ten.
`pakk` uses all of them, and its output is plain old zip, gzip, zstd, xz and bzip2 that every
other tool already reads.

```console
$ pakk pack rustdocs
✓ packed 51,996 files (637.20 MiB) into rustdocs.zip (146.30 MiB, 23%) in 1.99s

$ pakk unpack rustdocs.tar.gz
✓ unpacked 51,996 files (637.20 MiB) into ./rustdocs in 2.00s
```

## Benchmarks

The Rust documentation (637 MiB in 51,996 files of HTML, JS and images) on an Apple M4 with 10 cores.
Each figure is the median of 3 runs:

| Task | Classic tool | pakk | Speedup |
|---|---:|---:|---:|
| Create `.zip` | `zip -r` 9.21 s | **1.78 s** | **5.2×** |
| Create `.tar.gz` | `tar czf` 23.77 s | **1.33 s** | **17.9×** |
| Create `.tar.zst` | `tar \| zstd -T0` 17.93 s | **0.97 s** | **18.5×** |
| Create `.tar.xz` | `tar cJf` 79.11 s | **16.87 s** | **4.7×** |
| Create `.tar.bz2` | `tar cjf` 54.91 s | **6.79 s** | **8.1×** |
| Extract `.zip` | `unzip` 8.53 s | **2.42 s** | **3.5×** |
| Extract `.tar.gz` | `tar xzf` 4.63 s | **2.07 s** | **2.2×** |
| Extract `.tar.zst` | `zstd -dc \| tar x` 4.42 s | **2.08 s** | **2.1×** |
| Extract `.tar.xz` | `tar xJf` 6.05 s | **3.71 s** | **1.6×** |
| Extract `.tar.bz2` | `tar xjf` 10.90 s | **7.74 s** | **1.4×** |

The archives come out the same size or smaller: zip 146 vs 148 MiB, tar.gz 74 vs 80 MiB, tar.zst
51 vs 54 MiB, tar.xz 33.5 vs 33.9 MiB, tar.bz2 46 vs 48 MiB.

> [!NOTE]
> On macOS the system `tar` is bsdtar, which is slow at walking many small files, so part of the
> tar speedup comes from a better walker rather than from parallel compression alone. On Linux
> with GNU tar the gap is smaller. Run [`bench/bench.sh`](bench/bench.sh) to measure your own machine.

## Install

```sh
cargo install pakk
```

Or grab a prebuilt binary for Linux, macOS or Windows from the
[releases page](https://github.com/phlx0/pakk/releases). Each release comes with `SHA256SUMS`
and signed build provenance:

```sh
gh attestation verify pakk-v0.1.0-aarch64-apple-darwin.tar.gz --repo phlx0/pakk
```

### Build from source

You need [Rust](https://rustup.rs) 1.88 or newer.

```sh
git clone https://github.com/phlx0/pakk
cd pakk
cargo build --release          # binary lands in target/release/pakk
./target/release/pakk --help
cargo install --path .         # or put it on your PATH (~/.cargo/bin)
```

To run the checks CI runs:

```sh
cargo test                     # unit and end-to-end tests
cargo clippy --all-targets     # lints
cargo fmt --check              # formatting
bench/bench.sh                 # benchmarks against zip, tar and zstd (needs hyperfine)
```

## Usage

```sh
pakk pack photos/                 # → photos.zip
pakk pack src/ -f tar.zst         # → src.tar.zst
pakk pack a.txt b/ -o out.tar.gz  # several inputs, explicit output
pakk pack project/.               # contents only, no top-level folder
pakk pack logs/ -l 9              # maximum compression
pakk pack repo/ --gitignore       # skip everything git ignores
pakk pack site/ -x '*.map' -x node_modules

pakk unpack photos.zip            # → ./photos/
pakk unpack site.tar.gz -C /srv   # into a specific directory
pakk unpack pkg.tar.xz --strip-components 1
pakk unpack secret.zip -P hunter2 # or set PAKK_PASSWORD, or get prompted
pakk unpack app.log.gz            # plain .gz/.zst/.xz/.bz2 files work too

pakk list backup.tar.zst          # names
pakk ls -l backup.zip             # with sizes and dates
pakk test backup.tar.gz           # verify every checksum without extracting

pakk pack dir/ -f tar.zst -o - | ssh host 'pakk unpack - -C /dest'
curl -sL https://example.com/release.tar.gz | pakk unpack -
```

Short aliases: `p`/`c` for pack, `x`/`u` for unpack, `l`/`ls` for list, `t` for test.
`-j N` limits threads, `-v` prints every entry, `-q` silences output, `-k` never overwrites existing
files and `--xattrs` stores and restores extended attributes in tar archives.
Shell completions: `pakk completions zsh > ~/.zfunc/_pakk` (also bash, fish, elvish, powershell).

## What makes it fast

**zip: compress every file at once.** Each zip entry is independent, so pakk compresses them on
all cores. It then writes them in sorted order, so archives are reproducible. Memory stays bounded
because work is handed out in batches, and files over 128 MiB are streamed. Already-compressed
files (jpg, png, mp4, zip, …) are stored rather than deflated a second time, and any file that
would grow under compression is stored as well.

**zip: extract every file at once.** The central directory lists every entry up front, so pakk
plans the extraction and then decompresses entries in parallel through independent read cursors
on the same file.

**gzip: parallel in both directions while staying standard.** The stream is cut into 1 MiB blocks
and compressed in parallel as standard gzip members, which every gzip reader accepts. Each member
also records its own compressed size in a gzip extra field (`PZ`), which other tools ignore. When
pakk reads a `.tar.gz` it made, that field lets it find block boundaries without decompressing,
so it decompresses in parallel too. Any other `.tar.gz` falls back to normal sequential decoding.

**bzip2 and xz: independent blocks.** bzip2 data is compressed as parallel streams, the way
`pbzip2` does it. xz uses the format's own multi-block mode.

**tar: parallel reads and writes.** A tar stream is sequential, but the file system around it doesn't
have to be. When packing, pakk prefetches input files in parallel batches, so the SSD sees many
requests at once rather than one at a time. When extracting, file contents are handed to a pool of
writer threads, because creating thousands of small files is syscall-bound work that parallelizes well.

**zstd** uses its native multithreaded compressor.

## Safe by default

pakk extracts archives that often come from strangers, so it treats them as hostile. The guarantees
are listed in [SECURITY.md](SECURITY.md), and fuzz targets in `fuzz/` exercise them in CI.

- **No tarbombs.** `pakk unpack x.zip` extracts into `./x/`, unless the archive already has a
  single top-level folder, in which case that folder is used. Files never spill into your
  current directory, and without `-C` pakk never merges into an existing directory.
- **No writing outside the destination.** Entries with `..`, absolute paths or drive prefixes are
  rejected. pakk refuses to extract through a symlink that already exists in the destination,
  never writes through an existing hard link, and creates the archive's own symlinks only after
  every regular file is written.
- **No decompression bombs.** Memory use is bounded no matter what an archive claims about itself.
- **Atomic output.** Archives are written to a hidden temporary file and renamed into place when
  complete. Ctrl-C cleans up temporary files and staging directories.
- **No clobbering.** Existing archives are only overwritten with `--force`, and pakk refuses to
  overwrite one of its own inputs.
- **Integrity checked.** Every zip CRC, gzip block, zstd frame, xz check and bzip2 CRC is
  verified, including the checksum after the last tar entry.
- **Never hangs on special files.** FIFOs, sockets and devices are skipped with a warning, and input
  files are opened without following symlinks.
- **Exact timestamps.** Zip entries carry UTC modification times, so they survive time zones and
  odd seconds.
- **macOS clutter is skipped.** `__MACOSX/` folders and AppleDouble `._*` files are left out
  when extracting.
- **No `unsafe` code** in pakk itself (`unsafe_code = "forbid"`).

## Formats

| Format | Create | Extract | Parallel create | Parallel extract |
|---|:-:|:-:|:-:|:-:|
| `.zip` | ✓ | ✓ | ✓ | ✓ |
| `.tar.gz` / `.tgz` | ✓ | ✓ | ✓ | ✓ for pakk-made archives, writes for all |
| `.tar.zst` / `.tzst` | ✓ | ✓ | ✓ | writes |
| `.tar.xz` / `.txz` | ✓ | ✓ | ✓ | writes |
| `.tar.bz2` / `.tbz2` | ✓ | ✓ | ✓ | writes |
| `.tar` | ✓ | ✓ | reads | writes |
| `.gz` `.zst` `.xz` `.bz2` (single file) | | ✓ | | |

When extracting, the format is detected from the file contents, not the name. Zip entries
compressed with deflate, deflate64, bzip2, LZMA, XZ, zstd, PPMd or the legacy methods all extract,
including AES and ZipCrypto encrypted ones.

## Limitations

- pakk cannot create encrypted zips yet.
- Decompressing a `.tar.gz` made by another tool is inherently sequential (gzip has no block
  index). Only the file writes run in parallel. The same goes for xz and bzip2. pakk deliberately
  decodes xz sequentially with a memory limit, because parallel xz decoding trusts block sizes
  that a malicious archive controls.
- ACLs are not preserved. Extended attributes are, with `--xattrs` (tar formats only).

## License

Copyright (C) 2026 phlx0

pakk is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License](LICENSE) as published by the Free Software Foundation, either
version 2 of the License, or (at your option) any later version.
