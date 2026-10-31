# pakk

**Create and extract zip, tar.gz and tar.zst archives using every core you have.**

`zip`, `unzip`, `tar` and `gzip` were designed when computers had one core. Your laptop has ten.
`pakk` uses all of them, and its output is plain old zip and gzip that every other tool already reads.

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
| Create `.zip` | `zip -r` 9.21 s | **1.79 s** | **5.1×** |
| Create `.tar.gz` | `tar czf` 23.77 s | **1.54 s** | **15.4×** |
| Create `.tar.zst` | `tar \| zstd -T0` 17.93 s | **1.21 s** | **14.8×** |
| Extract `.zip` | `unzip` 8.53 s | **2.23 s** | **3.8×** |
| Extract `.tar.gz` | `tar xzf` 4.63 s | **2.10 s** | **2.2×** |
| Extract `.tar.zst` | `zstd -dc \| tar x` 4.42 s | **2.01 s** | **2.2×** |

The archives come out the same size or smaller: zip 146 vs 148 MiB, tar.gz 74 vs 80 MiB, tar.zst 51 vs 54 MiB.

> [!NOTE]
> On macOS the system `tar` is bsdtar, which is slow at walking many small files, so part of the
> tar speedup comes from a better walker rather than from parallel compression alone. On Linux
> with GNU tar the gap is smaller. Run [`bench/bench.sh`](bench/bench.sh) to measure your own machine.

## Install

```sh
cargo install pakk
```

Or grab a prebuilt binary for Linux, macOS or Windows from the
[releases page](https://github.com/phlx0/pakk/releases).

## Usage

```sh
pakk pack photos/                 # → photos.zip
pakk pack src/ -f tar.zst         # → src.tar.zst
pakk pack a.txt b/ -o out.tar.gz  # several inputs, explicit output
pakk pack project/.               # contents only, no top-level folder
pakk pack logs/ -l 9              # maximum compression

pakk unpack photos.zip            # → ./photos/
pakk unpack site.tar.gz -C /srv   # into a specific directory

pakk list backup.tar.zst          # names
pakk ls -l backup.zip             # with sizes and dates
```

Short aliases: `p`/`c` for pack, `x`/`u` for unpack, `l`/`ls` for list.
Use `-j N` to limit threads and `-q` to silence output.
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

**tar: parallel reads and writes.** A tar stream is sequential, but the file system around it doesn't
have to be. When packing, pakk prefetches input files in parallel batches, so the SSD sees many
requests at once rather than one at a time. When extracting, file contents are handed to a pool of
writer threads, because creating thousands of small files is syscall-bound work that parallelizes well.

**zstd** uses its native multithreaded compressor.

## Safe by default

- **No tarbombs.** `pakk unpack x.zip` extracts into `./x/`, unless the archive already has a
  single top-level folder, in which case that folder is used. Files never spill into your
  current directory, and without `-C` pakk never merges into an existing directory.
- **No zip-slip.** Entries with `..`, absolute paths or drive prefixes are rejected. Symlinks are
  created only after all regular files are written, so an archive can't plant a link and then
  write through it.
- **Atomic output.** Archives are written to a hidden temporary file and renamed into place when
  complete. A crash or Ctrl-C never leaves a half-written `photos.zip` behind.
- **No clobbering.** Existing archives are only overwritten with `--force`.
- **Integrity checked.** CRC32 is verified on every zip entry and gzip block, and zstd frames
  carry checksums.
- **macOS clutter is skipped.** `__MACOSX/` folders and AppleDouble `._*` files are left out
  when extracting.
- **No `unsafe` code** in pakk itself (`unsafe_code = "forbid"`).

## Formats

| Format | Create | Extract | Parallel create | Parallel extract |
|---|:-:|:-:|:-:|:-:|
| `.zip` | ✓ | ✓ | ✓ | ✓ |
| `.tar.gz` / `.tgz` | ✓ | ✓ | ✓ | ✓ for pakk-made archives, writes for all |
| `.tar.zst` / `.tzst` | ✓ | ✓ | ✓ | writes |
| `.tar` | ✓ | ✓ | reads | writes |

When extracting, the format is detected from the file contents, not the name.

## Limitations

- Encrypted zips are not supported.
- Decompressing a `.tar.gz` made by another tool is inherently sequential (gzip has no block
  index). Only the file writes run in parallel.
- Extended attributes and ACLs are not preserved.

## License

Copyright (C) 2026 phlx0

pakk is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License](LICENSE) as published by the Free Software Foundation, either
version 2 of the License, or (at your option) any later version.
