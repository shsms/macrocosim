//! Text-format primitives for a managed microgrid `.lisp` file: a
//! switchyard-generated block (structure, rewritten on every save)
//! followed by a hand-written script section (loaded verbatim,
//! never touched by switchyard). `parse` / `compose` split and
//! rejoin the two; `rewrite_id` patches the microgrid id inside the
//! generated block without disturbing anything else in the file.
//!
//! `render_block` / `render_empty_block` render the generated block
//! from live state, the inverse of `parse`. Between them these are
//! the whole read/write surface of the format: the loader
//! (`lisp::boot`) parses a file to decide how to evaluate it and
//! reads its head id back, the persist pass (`lisp::overrides`)
//! re-renders the block on every structural edit, undo / snapshots
//! swap blocks in and out, and the create / import / load-as
//! endpoints (`ui::handlers::microgrids`) compose and re-id whole
//! files.

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
;; microgrid's scope, on every load. Drive meters, define scenarios,
;; set setpoints here — do not construct components (the generated
;; block above owns the structure; constructing more here collides
;; on the next load).
";

/// The ownership comment written at the top of the script section
/// of a freshly created `enterprise.lisp`. Same two-section shape as
/// a microgrid file, but the tail runs once at boot for the whole
/// enterprise rather than per microgrid.
pub const FRESH_ENTERPRISE_SCRIPT_HEADER: &str = "\
;; Anything below is yours. It runs once at boot, before any
;; microgrid file — grid-frequency knobs and other enterprise-wide
;; settings live here.
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
/// flush, then rename over the target — a crash mid-write leaves
/// the old content in place rather than a truncated file.
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
    rewrite_head_kwarg(text, ":id", &new_id.to_string())?
        .ok_or_else(|| "no :id argument found in the (make-microgrid …) form".to_string())
}

/// Rewrite the `:grpc-port` argument of the `(make-microgrid …)`
/// form inside `text`'s generated block to `new_port`, leaving
/// everything else untouched.
///
/// A head with NO `:grpc-port` is returned unchanged: the loader
/// auto-allocates a free port for it already, so there is nothing to
/// fix. This pairs with [`rewrite_id`] on the copy path — a second
/// live microgrid needs its own port as much as its own id, because
/// the original's is held by a listening gRPC server.
pub fn rewrite_grpc_port(text: &str, new_port: u16) -> Result<String, String> {
    Ok(
        rewrite_head_kwarg(text, ":grpc-port", &new_port.to_string())?
            .unwrap_or_else(|| text.to_string()),
    )
}

/// The microgrid id declared by a generated `block` (the text
/// `parse` hands back in [`ParsedFile::generated`], markers already
/// stripped) — `None` when the block carries no `(make-microgrid …
/// :id N …)` head it can read.
///
/// The loader uses this to know which microgrid's scope the script
/// section belongs to when the block's own eval registered nothing
/// new, which is what a same-file reload looks like (the entry is
/// reused in place, so the registry key set doesn't move).
pub fn head_id(block: &str) -> Option<u64> {
    let span = head_kwarg_span(block, ":id").ok().flatten()?;
    block[span.0..span.1].trim().parse().ok()
}

/// Replace the value of `kwarg` in the `(make-microgrid …)` form of
/// `text`'s generated block with `value`, and return the whole file
/// text. `Ok(None)` means the form carries no such kwarg — the
/// caller decides whether that is an error or a no-op.
///
/// A byte splice over the original text rather than a re-render, so
/// every other kwarg, the component forms, the comments and the
/// hand-written script section come through exactly as they were.
fn rewrite_head_kwarg(text: &str, kwarg: &str, value: &str) -> Result<Option<String>, String> {
    let parsed = parse(text)?;
    let Some(block) = parsed.generated else {
        return Err(format!(
            "cannot rewrite {kwarg} in an unmanaged file — edit it by hand"
        ));
    };
    let Some((start, end)) = head_kwarg_span(&block, kwarg)? else {
        return Ok(None);
    };

    let mut new_block = String::with_capacity(block.len());
    new_block.push_str(&block[..start]);
    new_block.push_str(value);
    new_block.push_str(&block[end..]);

    Ok(Some(compose(&new_block, &parsed.script)))
}

/// Byte range of `kwarg`'s value atom inside a generated `block`, or
/// `None` when the `(make-microgrid …)` head carries no such kwarg.
/// Errors only when the block does not parse, or holds no
/// `(make-microgrid …)` form at all.
fn head_kwarg_span(block: &str, kwarg: &str) -> Result<Option<(usize, usize)>, String> {
    let cst = tulisp_fmt::parse(block).map_err(|e| format!("failed to parse block: {e:?}"))?;
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

    // The atom right after the keyword is its value. Only the head's
    // own atoms are walked — a nested `(%make-* :id …)` lives inside
    // a child List, so component ids can never be hit.
    let mut found_kw = false;
    Ok(form.iter().find_map(|c| {
        if found_kw {
            return match c {
                CstNode::Atom { span, .. } => Some((span.start, span.end)),
                _ => None,
            };
        }
        if matches!(c, CstNode::Atom { text, .. } if text == kwarg) {
            found_kw = true;
        }
        None
    }))
}

/// Render the generated block for a live microgrid: a
/// `(make-microgrid …)` form that reconstructs the definition, every
/// registered component (in `site.components()` order), and every
/// connection — such that re-evaluating the result reproduces the
/// same site (see the round-trip test below).
pub fn render_block(
    def: &crate::sim::microgrids::MicrogridDef,
    site: &crate::sim::MicrogridSite,
) -> String {
    use std::fmt::Write as _;

    let head = render_head(def);

    let mut body = String::new();
    for c in site.components().iter() {
        let id = c.id();
        write!(body, "\n    ({} :id {}", c.make_fn(), id).unwrap();
        if let Some(name) = site.name_override(id) {
            write!(
                body,
                " :name \"{}\"",
                crate::lisp::escape_lisp_string(&name)
            )
            .unwrap();
        }
        for (k, v) in c.constructor_kwargs() {
            write!(body, " {k} {v}").unwrap();
        }
        let mode = site.operational_mode(id);
        if mode != crate::sim::OperationalMode::Unspecified {
            write!(body, " :operational-mode '{mode}").unwrap();
        }
        body.push(')');
    }
    for (a, b) in site.all_connections() {
        write!(body, "\n    (connect {a} {b})").unwrap();
    }

    if body.is_empty() {
        format!("{head}\n  :topology\n  (lambda ()\n    nil))")
    } else {
        format!("{head}\n  :topology\n  (lambda (){body}))")
    }
}

/// Render the generated block for a microgrid with no components
/// yet — used by the create endpoint before any component exists.
pub fn render_empty_block(def: &crate::sim::microgrids::MicrogridDef) -> String {
    format!("{}\n  :topology\n  (lambda ()\n    nil))", render_head(def))
}

/// The `(make-microgrid :id … :name "…" :grpc-port … [:tso "…"]`
/// head shared by [`render_block`] and [`render_empty_block`],
/// without a trailing space or the `:topology` clause.
fn render_head(def: &crate::sim::microgrids::MicrogridDef) -> String {
    use std::fmt::Write as _;
    let mut head = format!(
        "(make-microgrid :id {} :name \"{}\" :grpc-port {}",
        def.id,
        crate::lisp::escape_lisp_string(&def.name),
        def.grpc_port,
    );
    if let Some(tso) = &def.tso {
        write!(head, " :tso \"{}\"", crate::lisp::escape_lisp_string(tso)).unwrap();
    }
    head
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
    fn rewrite_grpc_port_replaces_only_the_port() {
        let out = rewrite_grpc_port(MANAGED, 8899).unwrap();
        assert!(out.contains(":grpc-port 8899"), "{out}");
        assert!(
            out.contains("(make-microgrid :id 2201"),
            "the id is untouched"
        );
        assert!(
            out.contains("(%make-meter :id 5)"),
            "component forms untouched"
        );
        assert!(out.contains("(setq x 1)"), "script untouched");
        // Still one well-formed managed file.
        let parsed = parse(&out).unwrap();
        assert!(parsed.generated.is_some());
    }

    #[test]
    fn rewrite_grpc_port_leaves_a_portless_head_alone() {
        // No `:grpc-port` kwarg: the loader auto-allocates a free
        // port, so there is nothing to rewrite and nothing to fail.
        let text = compose(
            "(make-microgrid :id 3 :name \"p\"\n  :topology (lambda () nil))",
            ";; mine\n",
        );
        assert_eq!(rewrite_grpc_port(&text, 8899).unwrap(), text);
        // Unmanaged text is refused, same as rewrite_id.
        assert!(rewrite_grpc_port("(make-microgrid :id 1 :grpc-port 8800)", 8899).is_err());
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

    #[test]
    fn render_block_round_trips_through_a_fresh_config() {
        use super::super::test_support::config_with;
        let body = r#"
(make-microgrid :id 2205 :name "rt" :grpc-port 8815 :tso "TN"
  :topology
  (lambda ()
    (%make-grid-connection-point :id 1 :rated-fuse-current 100)
    (%make-meter :id 2 :name "main" :power 1500.0 :interval 500)
    (%make-meter :id 3 :hidden t)
    (%make-battery-inverter :id 4 :rated-lower -8000.0 :rated-upper 8000.0
                            :reactive-pf-limit 0)
    (%make-battery :id 5 :capacity 50000.0 :initial-soc 20.0)
    (%make-solar-inverter :id 6 :sunlight% 40.0)
    (%make-ev-charger :id 7)
    (%make-chp :id 8 :name "chp")
    (%make-meter :id 9 :operational-mode 'inactive)
    (connect 1 2) (connect 2 4) (connect 4 5)
    (connect 2 6) (connect 2 7) (connect 2 8) (connect 2 3) (connect 2 9)))
"#;
        let (cfg, _dir) = config_with(body);
        let (def, site) = {
            let reg = cfg.microgrids();
            let r = reg.lock();
            let e = r.get(&2205).unwrap();
            (e.def.clone(), e.site.clone())
        };
        let block = render_block(&def, &site);
        // Evaluate the rendered block in a second, fresh Config.
        let (cfg2, _dir2) = config_with(&block);
        let reg2 = cfg2.microgrids();
        let r2 = reg2.lock();
        let e2 = r2
            .get(&2205)
            .expect("rendered block re-registers the microgrid");
        assert_eq!(e2.def.name, "rt");
        assert_eq!(e2.def.grpc_port, 8815);
        assert_eq!(e2.def.tso.as_deref(), Some("TN"));
        // Same components, same constructor forms, same names, same edges.
        let sig = |site: &crate::sim::MicrogridSite| {
            let mut v: Vec<String> = site
                .components()
                .iter()
                .map(|c| {
                    format!(
                        "{} {} {:?} {:?} {:?}",
                        c.id(),
                        c.make_fn(),
                        c.constructor_kwargs(),
                        site.name_override(c.id()),
                        site.operational_mode(c.id())
                    )
                })
                .collect();
            v.sort();
            (v, site.all_connections())
        };
        assert_eq!(sig(&site), sig(&e2.site));
        // Rendering the reloaded site is byte-stable.
        assert_eq!(block, render_block(&e2.def, &e2.site));
    }
}
