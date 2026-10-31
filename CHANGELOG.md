# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0]

### Added

- `pack`, `unpack` and `list` for zip, tar, tar.gz and tar.zst.
- Parallel zip compression and extraction with reproducible entry order.
- Parallel gzip with an embedded block index (`PZ` extra field) for parallel decompression of
  pakk-made archives, while staying fully gzip-compatible.
- Parallel input prefetching and parallel file writes for tar formats.
- Smart extraction that never spills files into the current directory.
- Zip-slip protection, atomic output files and content-based format detection.
- Shell completions for bash, zsh, fish, elvish and PowerShell.
