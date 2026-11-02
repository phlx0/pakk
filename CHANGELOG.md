# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0]

### Added

- `pack`, `unpack`, `list` and `test` for zip, tar, tar.gz, tar.zst, tar.xz and tar.bz2, plus
  extraction of single `.gz`, `.zst`, `.xz` and `.bz2` files.
- Parallel zip compression and extraction with reproducible entry order.
- Parallel gzip with an embedded block index (`PZ` extra field) for parallel decompression of
  pakk-made archives, while staying fully gzip-compatible.
- Parallel bzip2 and multi-block xz compression.
- Parallel input prefetching and parallel file writes for tar formats.
- Smart extraction that never spills files into the current directory.
- `--exclude` globs and `--gitignore` when packing.
- `--strip-components`, `--keep` and `--verbose`.
- Reading from stdin and writing to stdout with `-`.
- Extraction of encrypted zips (AES and ZipCrypto) and of deflate64, bzip2, LZMA, XZ, zstd, PPMd
  and legacy zip entries.
- `--xattrs` to store and restore extended attributes in tar archives.
- Shell completions, a man page, a Homebrew formula template and release checksums with build
  provenance.

### Security

- Zip-slip protection, refusal to extract through existing symlinks or hard links, and bounded
  memory for decompression bombs.
- Atomic output files and cleanup of temporary files on Ctrl-C.
- Fuzz targets for gzip decoding and archive extraction.
