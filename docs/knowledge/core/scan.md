---
title: Scanning a repository for hidden instructions
summary: `--scan`: what it looks for, why its NUL binary rule differs from the loader's UTF-8 one, and the shape of the report.
refs: src/scan.rs, scan, Kind, Severity, Report, scan_file, src/loader.rs, read_file_data
---

`src/scan.rs` walks the repository, showing hidden files (a `.cursorrules` or a `.github/copilot-instructions.md` is exactly what it is for) and pruning only `target`, `.git`, `node_modules`, `vendor` and `bower_components`. It never follows a symlink; a symlink that resolves outside the repository is itself a finding.

A file counts as binary here when its first kilobyte holds a NUL. That is deliberately not the loader's rule: the loader asks whether the head is valid UTF-8, because it must show what it reads and a NUL is legal in a Rust literal, while `scan` answers "would a human call this a binary", where a NUL at all is the answer. Neither is wrong; they are different questions, and a test holds each.

The findings are the things a reader could miss: bidirectional control characters, tag characters and subdivision-flag payloads, runs of invisible or variation-selector characters, ANSI conceal bytes, private-use and control characters, invalid UTF-8, and prose that instructs an agent. Each is a `Kind` with a `Severity`: `Danger` when it is hidden from a human reader or unreadable as text, `Review` when it is suspicious or authoritative to an agent. A hidden message in tag characters is `Danger`; a genuine subdivision-flag emoji is only `Review`.

The report is markdown: what was covered (text, binary, non-UTF-8 and unreadable files), the two counts, then a table capped at `MAX_TABLE_ROWS` with the rest only counted. Paths and excerpts are escaped, so a hostile file name reads back as text in a terminal and in the markdown renderer. The command line's `scan` runs the same scan in place and shows its report in a report window, titled by how many danger findings it has.
