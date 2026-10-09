//! `/api/eval`, `/api/mg/{mg}/eval`, `/api/format` (tulisp-fmt) and
//! `/api/symbols`.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use tulisp::symbols::{ParamPosition, Signature, SymbolInfo, SymbolKind};

use crate::lisp::Config;
use crate::ui::api::{ApiError, Json, Mg, Query, Text};

#[derive(Serialize)]
pub(in crate::ui) struct EvalResponse {
    /// The evaluated form, printed.
    value: String,
}

/// Evaluate a Lisp expression with no microgrid in scope. Runs in
/// `spawn_blocking` because tulisp's `SharedMut` is std-sync-RwLock-
/// backed and grabbing the write lock from the executor thread would
/// stall every other tokio task waiting on that worker. 400 on an
/// evaluation error.
pub(in crate::ui) async fn eval(
    State(config): State<Config>,
    Text(body): Text,
) -> Result<Json<EvalResponse>, ApiError> {
    let value = super::blocking_with("eval task", move || config.eval(&body))
        .await?
        .map_err(ApiError::bad_request)?;
    Ok(Json(EvalResponse { value }))
}

/// Evaluate with the route's microgrid in scope. 404 when it is not
/// registered, including when a reload removes it while the eval
/// waits for the interpreter. The scope-set, eval, overrides append
/// and version bump share one interpreter-lock acquisition, so two
/// concurrent scoped evals can't cross microgrids.
pub(in crate::ui) async fn eval_for_mg(
    State(config): State<Config>,
    mg: Mg,
    Text(body): Text,
) -> Result<Json<EvalResponse>, ApiError> {
    let mg_id = mg.id;
    let value = super::blocking_with("eval task", move || {
        config.eval_in_registered_mg(mg_id, &body)
    })
    .await?
    .ok_or_else(|| ApiError::not_registered(mg_id))?
    .map_err(ApiError::bad_request)?;
    Ok(Json(EvalResponse { value }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct FormatQuery {
    /// Column budget for the formatter. Optional; defaults to 80.
    /// Clamped to a sane range so a stray client can't make
    /// `tulisp-fmt` chew through pathological inputs.
    width: Option<usize>,
}

/// Pretty-print a Lisp source string via `tulisp-fmt`. The body is
/// the raw source; the response is the formatted source as
/// text/plain. Returns 400 with the formatter's error message on
/// parse failure so the REPL can keep the user's input untouched
/// and surface the diagnostic.
pub(in crate::ui) async fn format(
    Query(q): Query<FormatQuery>,
    Text(body): Text,
) -> Result<String, ApiError> {
    let width = q.width.unwrap_or(80).clamp(20, 200);
    // spawn_blocking like every other CPU-bound handler: a large,
    // deeply nested body would otherwise stall a tokio worker.
    super::blocking(move || tulisp_fmt::format_with_width(&body, width))
        .await?
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

#[derive(Serialize)]
pub(in crate::ui) struct SymbolsResponse {
    symbols: Vec<SymbolEntry>,
}

/// One name the interpreter defines, for the REPL's completion popup and
/// signature hint.
#[derive(Serialize)]
pub(in crate::ui) struct SymbolEntry {
    name: String,
    /// `function`, `macro`, `special-form` or `variable`; `other` for a kind a
    /// later tulisp adds.
    kind: &'static str,
    /// `None` when tulisp has no signature for the name, as for a variable.
    signature: Option<SignatureEntry>,
    doc: Option<String>,
}

#[derive(Serialize)]
struct SignatureEntry {
    /// The signature as Emacs shows one: `(set-meter-power ID POWER-W)`.
    text: String,
    params: Vec<ParamEntry>,
}

#[derive(Serialize)]
struct ParamEntry {
    label: String,
    /// `required`, `optional`, `rest` or `key`; `other` for a position a later
    /// tulisp adds.
    position: &'static str,
    /// Where `label` is in `text`, in UTF-16 code units, as JavaScript indexes
    /// a string.
    start: usize,
    end: usize,
}

/// Every name the interpreter defines, sorted by name: the functions, macros
/// and special forms with their signatures, and the variables.  The list
/// follows what is loaded, so a `defun` in an eval shows up on the next call.
pub(in crate::ui) async fn symbols(
    State(config): State<Config>,
) -> Result<Json<SymbolsResponse>, ApiError> {
    let described = super::blocking(move || config.symbols()).await?;
    let mut symbols: Vec<SymbolEntry> = described
        .into_iter()
        .map(|(name, info)| symbol_entry(name, info))
        .collect();
    symbols.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(SymbolsResponse { symbols }))
}

fn symbol_entry(name: String, info: SymbolInfo) -> SymbolEntry {
    let signature = info.signature.map(|s| signature_entry(&name, &s));
    SymbolEntry {
        name,
        kind: match info.kind {
            SymbolKind::Function => "function",
            SymbolKind::Macro => "macro",
            SymbolKind::SpecialForm => "special-form",
            SymbolKind::Variable => "variable",
            _ => "other",
        },
        signature,
        doc: info.doc,
    }
}

fn signature_entry(name: &str, signature: &Signature) -> SignatureEntry {
    let (text, ranges) = signature.render_with_ranges(name);
    let utf16 = |byte: usize| text[..byte].encode_utf16().count();
    let params = signature
        .params
        .iter()
        .zip(ranges)
        .map(|(param, range)| ParamEntry {
            label: param.label(),
            position: match param.position {
                ParamPosition::Required => "required",
                ParamPosition::Optional => "optional",
                ParamPosition::Rest => "rest",
                ParamPosition::Keywords => "key",
                _ => "other",
            },
            start: utf16(range.start),
            end: utf16(range.end),
        })
        .collect();
    SignatureEntry { text, params }
}
