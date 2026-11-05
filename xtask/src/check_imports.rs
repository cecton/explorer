//! The repository's import rules, checked: a module-level `use` is refused unless it is
//! the one allowed glob (`use r3bl_tui::*;`) or a `std`/`core`/`alloc` import,
//! and `super::` and `self::` paths are refused outside a `#[cfg(test)]` module.
//!
//! The rules read the source as rustfmt writes it: a module-level item starts at column
//! zero, and the `}` that closes a module is the only one there.

use std::process::Command;

const ALLOWED: [&str; 4] = ["use std::", "use core::", "use alloc::", "use r3bl_tui::*;"];

pub(crate) fn run() -> xtask_watch::anyhow::Result<()> {
    let files = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "src",
        ])
        .output()?;
    let mut failures = Vec::new();
    let enums = enum_variants(&files.stdout)?;
    for file in String::from_utf8(files.stdout.clone())?
        .lines()
        .filter(|file| file.ends_with(".rs"))
    {
        let text = std::fs::read_to_string(file)?;
        let lines: Vec<&str> = text.lines().collect();
        let mut in_test_module = false;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let at_module_level = !line.starts_with(' ') && !line.starts_with('\t');
            if at_module_level && trimmed.starts_with("#[cfg(test)]") {
                let owns_a_body = lines
                    .get(index + 1)
                    .is_some_and(|next| next.starts_with("mod ") && next.trim_end().ends_with('{'));
                if owns_a_body {
                    in_test_module = true;
                }
            }
            if in_test_module && at_module_level && trimmed == "}" {
                in_test_module = false;
                continue;
            }
            let is_use = at_module_level && is_module_level_use(trimmed);
            let is_any_use = strip_visibility(trimmed).starts_with("use ");
            if is_use && !in_test_module && !ALLOWED.iter().any(|a| trimmed.starts_with(a)) {
                failures.push(format!(
                    "{file}:{}: module-level `use` — spell the name out in full, importing \
                     only `r3bl_tui`, `std`, `core` and `alloc` at the top of a file",
                    index + 1
                ));
            }
            // `use super::*;` opening a test module is the one allowed `super::` path.
            let is_test_glob = is_use && trimmed == "use super::*;";
            if !in_test_module && !is_test_glob && has_relative_path(trimmed) {
                failures.push(format!(
                    "{file}:{}: `super::` or `self::` path — spell the path from `crate::`",
                    index + 1
                ));
            }
            // A single-variant import (`use crate::git::RefKind::Branch;`) confuses two
            // enums that share the variant name; the enum is imported instead.
            if is_any_use && imports_one_variant(trimmed) {
                failures.push(format!(
                    "{file}:{}: single-variant import — import the enum and write \
                     `RefKind::Branch`",
                    index + 1
                ));
            }
        }
        failures.extend(long_variant_paths(file, &text, &enums));
    }
    if failures.is_empty() {
        println!("imports: clean");
        return Ok(());
    }
    for failure in &failures {
        eprintln!("{failure}");
    }
    xtask_watch::anyhow::bail!("{} import rule violations", failures.len())
}

fn is_module_level_use(line: &str) -> bool {
    strip_visibility(line).starts_with("use ")
}

fn strip_visibility(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("pub ") {
        return rest;
    }
    if let Some(rest) = line.strip_prefix("pub(")
        && let Some(close) = rest.find(')')
    {
        return rest[close + 1..].trim_start();
    }
    line
}

fn has_relative_path(line: &str) -> bool {
    let code = strip_visibility(line).trim_start();
    let code = code
        .strip_prefix("use ")
        .map(str::trim_start)
        .unwrap_or(code);
    if code.starts_with("super::") || code.starts_with("self::") {
        return true;
    }
    [
        " super::", " self::", "(super::", "(self::", ",super::", ", self::", "{super::",
        "{ self::",
    ]
    .iter()
    .any(|needle| code.contains(needle))
}

/// `use crate::…::Enum::Variant;` at module level or in a body.
fn imports_one_variant(line: &str) -> bool {
    let code = strip_visibility(line)
        .strip_prefix("use ")
        .map(str::trim)
        .unwrap_or("");
    // `use crate::a::{B::C, D::E};` imports variants too: read past the braces.
    let code = code.trim_end_matches(';').replace(['{', '}'], "");
    let segs: Vec<&str> = code.split("::").map(str::trim).collect();
    segs.len() >= 4
        && segs[0] == "crate"
        && segs[segs.len() - 1]
            .chars()
            .next()
            .is_some_and(|c| c.is_uppercase())
        && segs[segs.len() - 2]
            .chars()
            .next()
            .is_some_and(|c| c.is_uppercase())
}

/// Every variant name this crate's enums declare, so that a variant can be told from a
/// type that happens to share its name.
fn enum_variants(stdout: &[u8]) -> xtask_watch::anyhow::Result<std::collections::HashSet<String>> {
    let mut variants = std::collections::HashSet::new();
    for file in String::from_utf8(stdout.to_vec())?
        .lines()
        .filter(|file| file.ends_with(".rs"))
    {
        let text = std::fs::read_to_string(file)?;
        let mut inside = false;
        let mut depth = 0i32;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if !inside && line.ends_with('{') && strip_visibility(trimmed).starts_with("enum ") {
                inside = true;
            } else if inside {
                let name: String = trimmed
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if name.chars().next().is_some_and(|c| c.is_uppercase())
                    && line.starts_with("    ")
                    && !trimmed.starts_with('#')
                {
                    variants.insert(name);
                }
            }
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if inside && depth <= 0 {
                inside = false;
            }
        }
    }
    Ok(variants)
}

/// A variant spelled through a full path inside a function body, where the enum is
/// imported and `Enum::Variant` says as much. Signatures keep the full path.
fn long_variant_paths(
    file: &str,
    text: &str,
    variants: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (open, close) in function_bodies(text) {
        let chars: Vec<char> = text.chars().collect();
        let body: String = chars[open + 1..close].iter().collect();
        let first_line = chars[..open].iter().filter(|c| **c == '\n').count() + 1;
        for (offset, line) in body.lines().enumerate() {
            let code = mask_line(line);
            if code.trim_start().starts_with("use ") {
                continue;
            }
            for (path, _) in crate_paths(&code) {
                let segments: Vec<&str> = path.split("::").collect();
                if segments.len() < 4 {
                    continue;
                }
                let tail = segments[segments.len() - 1];
                let owner = segments[segments.len() - 2];
                // `Window` is both an enum and a variant of another enum, so the rule
                // cannot lean on "the owner is not a variant": what distinguishes a
                // variant path is the tail being a variant of the owner, which the
                // source alone cannot tell. What it can tell is the shape: a CamelCase
                // segment followed by a CamelCase segment, at least four deep.
                let owner_is_type = owner.starts_with(|c: char| c.is_uppercase());
                if variants.contains(tail) && owner_is_type {
                    failures.push(format!(
                        "{file}:{}: `{path}` — import the enum and write `{owner}::{tail}`",
                        first_line + offset
                    ));
                }
            }
        }
    }
    failures
}

/// (open, close) character offsets of every function body in `text`.
fn function_bodies(text: &str) -> Vec<(usize, usize)> {
    let masked: Vec<char> = mask_line_joined(text).chars().collect();
    let mut bodies = Vec::new();
    let mut i = 0;
    while i + 2 < masked.len() {
        let at_fn = masked[i] == 'f'
            && masked[i + 1] == 'n'
            && (i == 0 || !is_ident_char(masked[i - 1]))
            && masked.get(i + 2).is_none_or(|c| !is_ident_char(*c));
        if !at_fn {
            i += 1;
            continue;
        }
        let mut depth = 0i32;
        let mut j = i + 2;
        let mut found = None;
        while j < masked.len() {
            match masked[j] {
                '(' | '[' => depth += 1,
                // Never below zero: a stray `)` in a signature must not hide the brace.
                ')' | ']' => depth = (depth - 1).max(0),
                ';' if depth == 0 => break,
                '{' if depth == 0 => {
                    found = Some(j);
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        if let Some(open) = found
            && let Some(close) = matching_brace(&masked, open)
        {
            bodies.push((open, close));
            i = close;
            continue;
        }
        i += 1;
    }
    bodies
}

fn matching_brace(masked: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, c) in masked.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// The whole source with comments and string literals blanked, newlines kept.
fn mask_line_joined(text: &str) -> String {
    text.lines().map(mask_line).collect::<Vec<_>>().join("\n")
}

/// Every `crate::…` path on a line.
fn crate_paths(line: &str) -> Vec<(String, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 7 <= chars.len() {
        if chars[i..].starts_with(&['c', 'r', 'a', 't', 'e', ':', ':'])
            && (i == 0 || !is_ident_char(chars[i - 1]))
        {
            let mut end = i + 7;
            loop {
                if end < chars.len() && is_ident_char(chars[end]) {
                    end += 1;
                } else if end + 1 < chars.len() && chars[end] == ':' && chars[end + 1] == ':' {
                    end += 2;
                } else {
                    break;
                }
            }
            let path: String = chars[i..end].iter().collect();
            out.push((path.trim_end_matches(':').to_string(), i));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A line with its comment and string literals blanked, so their braces do not count.
fn mask_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            out.push(' ');
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(' ');
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            // The rest of the line is a comment: blank it out, keeping the line's length
            // so that offsets computed on the masked text still index the original.
            let rest = 1 + chars.count();
            out.extend(std::iter::repeat_n(' ', rest));
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_body_whose_signature_spans_lines() {
        let text = "fn a(\n    x: u8,\n) -> Option<u8> {\n    let y = 1;\n}\n";
        let bodies = function_bodies(text);
        assert_eq!(bodies.len(), 1);
        let (open, close) = bodies[0];
        let chars: Vec<char> = text.chars().collect();
        let body: String = chars[open + 1..close].iter().collect();
        assert!(body.contains("let y = 1;"), "{body:?}");
    }

    #[test]
    fn finds_a_variant_path_inside_a_body() {
        let text = "fn a() -> Option<usize> {\n    match w {\n        crate::m::Window::Report(id) => Some(id),\n        _ => None,\n    }\n}\n";
        let mut variants = std::collections::HashSet::new();
        variants.insert("Report".to_string());
        let failures = super::long_variant_paths("t.rs", text, &variants);
        assert_eq!(failures.len(), 1, "{failures:?}");
    }
}
#[cfg(test)]
mod joined_tests {
    #[test]
    fn joined_masks_doc_comments() {
        let text = std::fs::read_to_string("../src/tui/report_pane.rs").unwrap();
        let masked = super::mask_line_joined(&text);
        assert!(
            !masked.contains("of the conversation"),
            "doc comment leaked"
        );
        assert!(masked.contains("pub struct ReportComponent"), "code kept");
    }
}

#[cfg(test)]
mod variant_tests {
    /// A `pub(crate) enum` declares variants like any other: only `enum ` and
    /// `pub enum ` were read once, so five enums' variants escaped the body rule.
    #[test]
    fn a_restricted_enum_declares_variants_too() {
        let now = std::time::SystemTime::now();
        let dir = std::env::temp_dir().join(format!(
            "imports-{}",
            now.duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("probe.rs");
        std::fs::write(&file, "pub(crate) enum Fold {\n    Expanded,\n}\n").unwrap();
        let listing = format!("{}\n", file.display());
        let variants = super::enum_variants(listing.as_bytes()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(variants.contains("Expanded"), "{variants:?}");
    }

    /// The rule needs a real function body: a struct, an `impl` and a signature that
    /// spans lines all sit between the `fn` and the brace it opens.
    #[test]
    fn a_variant_path_inside_a_body_is_reported() {
        let text = "impl X {\n    fn a(&self, w: W) -> Option<usize> {\n        match w {\n            crate::m::Window::Report(id) => Some(id),\n            _ => None,\n        }\n    }\n}\n";
        let mut variants = std::collections::HashSet::new();
        variants.insert("Report".to_string());
        let failures = super::long_variant_paths("t.rs", text, &variants);
        assert_eq!(failures.len(), 1, "{failures:?}");
    }

    /// A body imports a variant the same way a module level does.
    #[test]
    fn a_single_variant_import_inside_a_body_is_reported() {
        assert!(super::imports_one_variant(
            "use crate::git::{RefKind::Branch};"
        ));
        assert!(super::imports_one_variant(
            "use crate::git::RefKind::Branch;"
        ));
        assert!(!super::imports_one_variant("use crate::git::RefKind;"));
    }

    /// The line a body failure names is the line the path sits on.
    #[test]
    fn a_variant_path_is_reported_on_its_own_line() {
        let text = "fn a() -> Option<usize> {\n    let x = 1;\n    match w {\n        crate::m::Window::Report(id) => Some(id),\n        _ => None,\n    }\n}\n";
        let mut variants = std::collections::HashSet::new();
        variants.insert("Report".to_string());
        let failures = super::long_variant_paths("t.rs", text, &variants);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].starts_with("t.rs:4:"), "{failures:?}");
    }

    /// A signature keeps the full path, where `use` cannot reach.
    #[test]
    fn a_variant_path_in_a_signature_is_left_alone() {
        let text = "fn a() -> Option<crate::m::Window::Report> {\n    None\n}\n";
        let mut variants = std::collections::HashSet::new();
        variants.insert("Report".to_string());
        let failures = super::long_variant_paths("t.rs", text, &variants);
        assert!(failures.is_empty(), "{failures:?}");
    }
}
