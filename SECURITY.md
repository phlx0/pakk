# Security policy

pakk extracts archives that often come from untrusted sources, so security reports are very welcome.

## Reporting a vulnerability

Please **do not open a public issue**. Report privately through
[GitHub security advisories](https://github.com/phlx0/pakk/security/advisories/new).
You will get a response within a few days, and a fix plus advisory once the issue is confirmed.

## What pakk guarantees

When extracting, pakk must never:

- write outside the destination directory (`..`, absolute paths, drive prefixes, existing symlinks
  or hard links in the destination),
- follow a symlink that the archive itself created,
- use unbounded memory for a small input (decompression bombs),
- hang or crash on malformed input.

When packing, pakk must never follow symlinks, block on special files, or replace one of its inputs.

A violation of any of these is a security bug. The `fuzz/` targets exercise the extraction
guarantees and run in CI.

## Supported versions

Only the latest release receives security fixes.
