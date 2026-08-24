//! Text-format primitives for a managed microgrid `.lisp` file: a
//! switchyard-generated block (structure, rewritten on every save)
//! followed by a hand-written script section (loaded verbatim,
//! never touched by switchyard). `parse` / `compose` split and
//! rejoin the two; `rewrite_id` patches the microgrid id inside the
//! generated block without disturbing anything else in the file.
//!
//! Nothing outside this module depends on it yet — later tasks add
//! rendering the generated block from live state and the loader
//! that reads these files back in.

use std::fs;
use std::io::Write;
use std::path::Path;

use tulisp_fmt::cst::CstNode;

/// Marks the start of the switchyard-generated block. Used to
/// detect a managed file (the first non-empty line starts with
/// this) — `GENERATED_BEGIN_LINE` is the exact line switchyard
/// writes, which carries an explanatory suffix beyond this prefix.
pub const GENERATED_BEGIN: &str = ";;; switchyard:generated";

/// The literal first line of the generated block, as switchyard
/// writes it.
pub const GENERATED_BEGIN_LINE: &str =
    ";;; switchyard:generated — rewritten by switchyard, do not edit";

/// Marks the end of the switchyard-generated block.
pub const GENERATED_END: &str = ";;; switchyard:end";

/// The ownership comment written at the top of the script section
/// of a freshly created managed file.
pub const FRESH_SCRIPT_HEADER: &str = "\
;; Anything below is yours. It runs after the structure, in this
;; microgrid's scope, on every load.
";

/// A microgrid file split into its two sections.
pub struct ParsedFile {
    /// The generated block's contents, markers stripped, when the
    /// file is managed (its first non-empty line is
    /// `GENERATED_BEGIN_LINE`). `None` for an unmanaged file —
    /// hand-written, with no switchyard markers at all.
    pub generated: Option<String>,
    /// Everything after the generated block (managed file), or the
    /// entire file (unmanaged file), byte-for-byte.
    pub script: String,
}

/// Split `text` into its generated block and script section.
///
/// A file is managed when its first non-empty line starts with
/// `GENERATED_BEGIN`; unmanaged text with no marker anywhere is
/// returned as `generated: None, script: text`. A marker found
/// anywhere other than a well-formed leading block is an error —
/// this only happens to text switchyard itself never wrote, so
/// failing loudly beats silently mangling it.
pub fn parse(text: &str) -> Result<ParsedFile, String> {
    let first_non_empty = text.lines().find(|l| !l.trim().is_empty());
    let is_managed = first_non_empty.is_some_and(|l| l.starts_with(GENERATED_BEGIN));

    if !is_managed {
        for line in text.lines() {
            if line.starts_with(GENERATED_BEGIN) || line.starts_with(GENERATED_END) {
                return Err("switchyard marker found but not at the top of the file".to_string());
            }
        }
        return Ok(ParsedFile {
            generated: None,
            script: text.to_string(),
        });
    }

    // Walk line-by-line via byte offsets (not `lines().collect()` +
    // rejoin) so the script tail survives byte-identical, including
    // any trailing-newline quirks of the original file.
    let mut generated_start = None;
    let mut end_line_start = None;
    let mut end_line_end = None;

    for (start, end, next) in line_spans(text) {
        let line = &text[start..end];
        match generated_start {
            None => {
                if line.starts_with(GENERATED_BEGIN) {
                    generated_start = Some(next);
                }
                // else: a blank line before the begin marker — skip.
            }
            Some(_) => {
                if line.starts_with(GENERATED_END) {
                    end_line_start = Some(start);
                    end_line_end = Some(next);
                    break;
                } else if line.starts_with(GENERATED_BEGIN) {
                    return Err(
                        "a second 'switchyard:generated' marker found inside the block".into(),
                    );
                }
            }
        }
    }

    let Some(end_start) = end_line_start else {
        return Err("missing ';;; switchyard:end' marker".to_string());
    };

    Ok(ParsedFile {
        generated: Some(text[generated_start.unwrap()..end_start].to_string()),
        script: text[end_line_end.unwrap()..].to_string(),
    })
}

/// Yield `(line_start, line_end_excluding_newline, next_line_start)`
/// byte offsets for every line in `text`, including a final line
/// with no trailing newline. Used instead of `str::lines()` +
/// rejoin so callers can slice the original bytes exactly.
fn line_spans(text: &str) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    let len = text.len();
    let mut pos = 0usize;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let end = text[pos..].find('\n').map_or(len, |rel| pos + rel);
        let next = if end < len { end + 1 } else { len };
        let span = (pos, end, next);
        if next == len {
            done = true;
        }
        pos = next;
        Some(span)
    })
}

/// Join a generated block and a script section back into one
/// managed file's text. `block` is trimmed of trailing newlines
/// before the end marker is appended, so `compose` never emits a
/// blank line between the last generated form and
/// `GENERATED_END`.
pub fn compose(generated_block: &str, script: &str) -> String {
    let block = generated_block.trim_end_matches('\n');
    format!("{GENERATED_BEGIN_LINE}\n{block}\n{GENERATED_END}\n{script}")
}

/// Write `text` to `path` atomically: write to a `.tmp` sibling,
/// flush, then rename over the target. Mirrors
/// `Config::replace_overrides_text_locked` — a crash mid-write
/// leaves the old content in place rather than a truncated file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("lisp.tmp");
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Rewrite the `:id` argument of the `(make-microgrid …)` form
/// inside `text`'s generated block to `new_id`, leaving everything
/// else — component ids, formatting, the script section — untouched.
///
/// Errors on unmanaged text (nothing to rewrite) or a generated
/// block that doesn't parse as a `(make-microgrid :id … …)` form.
pub fn rewrite_id(text: &str, new_id: u64) -> Result<String, String> {
    let parsed = parse(text)?;
    let Some(block) = parsed.generated else {
        return Err("cannot rewrite the id of an unmanaged file — edit it by hand".to_string());
    };

    let cst = tulisp_fmt::parse(&block).map_err(|e| format!("failed to parse block: {e:?}"))?;
    let form = cst
        .nodes
        .iter()
        .find_map(|n| match n {
            CstNode::List { children, .. } => {
                let first_atom = children.iter().find_map(|c| match c {
                    CstNode::Atom { text, .. } => Some(text.as_str()),
                    _ => None,
                });
                (first_atom == Some("make-microgrid")).then_some(children)
            }
            _ => None,
        })
        .ok_or_else(|| "no (make-microgrid …) form found in the generated block".to_string())?;

    let mut found_id_kw = false;
    let id_span = form
        .iter()
        .find_map(|c| {
            if found_id_kw {
                return match c {
                    CstNode::Atom { span, .. } => Some(span.clone()),
                    _ => None,
                };
            }
            if matches!(c, CstNode::Atom { text, .. } if text == ":id") {
                found_id_kw = true;
            }
            None
        })
        .ok_or_else(|| "no :id argument found in the (make-microgrid …) form".to_string())?;

    let mut new_block = String::with_capacity(block.len());
    new_block.push_str(&block[..id_span.start]);
    new_block.push_str(&new_id.to_string());
    new_block.push_str(&block[id_span.end..]);

    Ok(compose(&new_block, &parsed.script))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANAGED: &str = "\
;;; switchyard:generated — rewritten by switchyard, do not edit
(make-microgrid :id 2201 :name \"a\" :grpc-port 8810
  :topology
  (lambda ()
    (%make-meter :id 5)))
;;; switchyard:end
;; Anything below is yours.
(setq x 1)
";

    #[test]
    fn parse_splits_managed_file_into_block_and_script() {
        let p = parse(MANAGED).unwrap();
        let block = p.generated.expect("managed");
        assert!(block.contains("(make-microgrid :id 2201"));
        assert!(!block.contains("switchyard:generated"));
        assert!(!block.contains("switchyard:end"));
        assert!(p.script.contains("(setq x 1)"));
        assert!(!p.script.contains("make-microgrid"));
    }

    #[test]
    fn parse_treats_markerless_text_as_unmanaged_script() {
        let p = parse("(make-microgrid :id 1 :topology (lambda () nil))\n").unwrap();
        assert!(p.generated.is_none());
        assert!(p.script.contains("make-microgrid"));
    }

    #[test]
    fn parse_rejects_marker_not_at_top_and_missing_end() {
        assert!(parse("(setq x 1)\n;;; switchyard:generated\nnil\n;;; switchyard:end\n").is_err());
        assert!(parse(";;; switchyard:generated\n(make-microgrid :id 1)\n").is_err());
        // A second begin inside the block is an error too.
        assert!(
            parse(";;; switchyard:generated\n;;; switchyard:generated\nnil\n;;; switchyard:end\n")
                .is_err()
        );
    }

    #[test]
    fn compose_then_parse_round_trips() {
        let block = "(make-microgrid :id 7 :grpc-port 8807\n  :topology (lambda () nil))";
        let script = ";; mine\n(setq y 2)\n";
        let text = compose(block, script);
        assert!(text.starts_with(GENERATED_BEGIN));
        let p = parse(&text).unwrap();
        assert_eq!(p.generated.as_deref().map(str::trim), Some(block.trim()));
        assert_eq!(p.script, script);
    }

    #[test]
    fn rewrite_id_replaces_only_the_microgrid_id() {
        let out = rewrite_id(MANAGED, 2299).unwrap();
        assert!(out.contains("(make-microgrid :id 2299"));
        assert!(
            out.contains("(%make-meter :id 5)"),
            "component ids untouched"
        );
        assert!(out.contains("(setq x 1)"), "script untouched");
        // Unmanaged text is refused.
        assert!(rewrite_id("(make-microgrid :id 1)", 2).is_err());
    }

    #[test]
    fn write_atomic_replaces_content() {
        let dir = std::env::temp_dir().join(format!("sw-mgfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.lisp");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("lisp.tmp").exists());
    }
}
