//! Serves [`workon_annotations::store::AnnotationStore`] over MCP so an agent can read and
//! write the same comment/walkthrough substrate the review TUI renders (ADR-039).
//!
//! Each tool call opens its own [`AnnotationStore`] and, where needed, its own
//! `git2::Repository`: neither the sqlite connection nor a `Repository` is `Sync`, and rmcp
//! dispatches tool calls concurrently, so nothing here is held across calls.

use std::path::Path;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use workon_annotations::store::{AnnotationStore, TourStop, Walkthrough};
use workon_annotations::{Anchor, AnnotationKind, ChangesetKey, NewAnnotation, Status};

use crate::server::WorkonServer;

#[tool_router(router = tool_router, vis = "pub(crate)")]
impl WorkonServer {
    #[tool(
        description = "List annotations for a changeset, optionally filtered to one file \
        path. Each entry's anchor is re-resolved against current content and reports how it \
        resolved (exact / shifted / orphaned)."
    )]
    async fn annotation_list(
        &self,
        Parameters(args): Parameters<ListArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        let key = ChangesetKey::new(args.changeset, args.uncommitted);

        let annotations = match &args.path {
            Some(path) => store.by_path(&key, path).map_err(store_err)?,
            None => store.by_changeset(&key).map_err(store_err)?,
        };

        let out: Vec<Value> = annotations
            .into_iter()
            .map(|annotation| {
                let resolution = annotation.anchor.as_ref().map(|anchor| {
                    match read_lines(&repo, &annotation.changeset, &anchor.path) {
                        Ok(lines) => {
                            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                            workon_annotations::anchor::resolve(anchor, &refs)
                        }
                        Err(_) => workon_annotations::anchor::Resolution {
                            lineno: None,
                            anchoring: workon_annotations::Anchoring::Orphaned,
                        },
                    }
                });
                annotation_to_json(&annotation, resolution)
            })
            .collect();

        to_json_string(&out)
    }

    #[tool(description = "Fetch one annotation by uid.")]
    async fn annotation_get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        let annotation = get_or_not_found(&store, &args.uid)?;
        to_json_string(&annotation_to_json(&annotation, None))
    }

    #[tool(
        description = "Create a comment, tour stop, or chapter anchored to a file and \
        line. The server reads the target line and 3 lines of context each way itself, from \
        the worktree for the uncommitted changeset or from the changeset branch's tip tree \
        for a committed one."
    )]
    async fn annotation_post(
        &self,
        Parameters(args): Parameters<PostArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        let key = ChangesetKey::new(args.changeset, args.uncommitted);
        let new_side = parse_side(&args.side)?;
        let lines = read_lines(&repo, &key, &args.path)?;
        let anchor = build_anchor(&lines, &args.path, new_side, args.line)?;
        let kind = parse_kind(args.kind.as_deref())?;

        let annotation = store
            .insert(NewAnnotation {
                kind,
                changeset: key,
                anchor: Some(anchor),
                body: args.body,
                author: args.author,
                tour: args.tour,
                seq: args.seq,
            })
            .map_err(store_err)?;
        to_json_string(&annotation_to_json(&annotation, None))
    }

    #[tool(
        description = "Reply to an existing annotation. A reply has no anchor of its own \
        — it inherits the parent's location."
    )]
    async fn annotation_reply(
        &self,
        Parameters(args): Parameters<ReplyArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        let annotation = store
            .reply(&args.parent_uid, &args.body, &args.author)
            .map_err(store_err)?;
        to_json_string(&annotation_to_json(&annotation, None))
    }

    #[tool(
        description = "Set an annotation's status. `resolved: true` (the default) \
        resolves it; `resolved: false` reopens it."
    )]
    async fn annotation_resolve(
        &self,
        Parameters(args): Parameters<ResolveArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        let status = if args.resolved {
            Status::Resolved
        } else {
            Status::Open
        };
        store.set_status(&args.uid, status).map_err(store_err)?;
        let annotation = get_or_not_found(&store, &args.uid)?;
        to_json_string(&annotation_to_json(&annotation, None))
    }

    #[tool(
        description = "Replace an annotation's body text (e.g. revising a walkthrough stop or \
        a redline note). The uid stays stable; updated_at moves."
    )]
    async fn annotation_update(
        &self,
        Parameters(args): Parameters<UpdateArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        store
            .update_body(&args.uid, &args.body)
            .map_err(store_err)?;
        let annotation = get_or_not_found(&store, &args.uid)?;
        to_json_string(&annotation_to_json(&annotation, None))
    }

    #[tool(description = "Delete an annotation and (transitively) every reply to it.")]
    async fn annotation_delete(
        &self,
        Parameters(args): Parameters<DeleteArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let store = open_store(&repo)?;
        store.delete(&args.uid).map_err(store_err)?;
        Ok(json!({ "deleted": args.uid }).to_string())
    }

    #[tool(
        description = "Write a whole walkthrough — an optional per-changeset chapter plus \
        ordered tour stops — in one transaction, so the TUI's watcher never observes a \
        half-authored tour."
    )]
    async fn walkthrough_put(
        &self,
        Parameters(args): Parameters<WalkthroughPutArgs>,
    ) -> Result<String, McpError> {
        let repo = discover_repo(args.repo_path.as_deref())?;
        let key = ChangesetKey::new(args.changeset, args.uncommitted);

        let mut stops = Vec::with_capacity(args.stops.len());
        for stop in args.stops {
            let new_side = parse_side(&stop.side)?;
            let lines = read_lines(&repo, &key, &stop.path)?;
            let anchor = build_anchor(&lines, &stop.path, new_side, stop.line)?;
            stops.push(TourStop {
                anchor,
                body: stop.body,
                author: stop.author,
                seq: stop.seq,
            });
        }

        let store = open_store(&repo)?;
        store
            .put_walkthrough(Walkthrough {
                changeset: key,
                tour: args.tour,
                chapter: args.chapter,
                chapter_author: args.chapter_author,
                stops,
            })
            .map_err(store_err)?;
        Ok(json!({ "ok": true }).to_string())
    }
}

// --- Tool argument shapes -------------------------------------------------------------
//
// Every args struct carries an optional `repo_path`; `Repository::discover` resolves the
// current directory when it's absent, matching how `git` subcommands find their repo.

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ListArgs {
    /// Repository path to resolve from; defaults to discovering from the current directory.
    repo_path: Option<String>,
    /// Branch name identifying the changeset.
    changeset: String,
    /// Whether `changeset` names its uncommitted layer rather than its committed tip.
    #[serde(default)]
    uncommitted: bool,
    /// Restrict to annotations anchored to this file path.
    path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct GetArgs {
    repo_path: Option<String>,
    uid: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PostArgs {
    repo_path: Option<String>,
    changeset: String,
    #[serde(default)]
    uncommitted: bool,
    /// File path the annotation anchors to, relative to the repository root.
    path: String,
    /// Which side of the diff to anchor: `"new"` or `"old"`.
    side: String,
    /// 1-based line number on that side.
    line: u32,
    body: String,
    author: String,
    /// `"comment"` (default), `"tour_stop"`, or `"chapter"`.
    kind: Option<String>,
    /// Tour name, required for `kind: "tour_stop"`.
    tour: Option<String>,
    /// Order within the tour, required for `kind: "tour_stop"`.
    seq: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ReplyArgs {
    repo_path: Option<String>,
    parent_uid: String,
    body: String,
    author: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ResolveArgs {
    repo_path: Option<String>,
    uid: String,
    #[serde(default = "default_true")]
    resolved: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct UpdateArgs {
    repo_path: Option<String>,
    uid: String,
    body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DeleteArgs {
    repo_path: Option<String>,
    uid: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct StopArgs {
    path: String,
    side: String,
    line: u32,
    body: String,
    author: String,
    seq: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct WalkthroughPutArgs {
    repo_path: Option<String>,
    changeset: String,
    #[serde(default)]
    uncommitted: bool,
    tour: String,
    chapter: Option<String>,
    chapter_author: Option<String>,
    #[serde(default)]
    stops: Vec<StopArgs>,
}

// --- Repo/store plumbing --------------------------------------------------------------

fn discover_repo(repo_path: Option<&str>) -> Result<git2::Repository, McpError> {
    git2::Repository::discover(repo_path.unwrap_or("."))
        .map_err(|source| mcp_err("workon::annotations::mcp::repo_discover_failed", source))
}

fn open_store(repo: &git2::Repository) -> Result<AnnotationStore, McpError> {
    AnnotationStore::open(repo.commondir()).map_err(store_err)
}

fn get_or_not_found(
    store: &AnnotationStore,
    uid: &str,
) -> Result<workon_annotations::Annotation, McpError> {
    store.get(uid).map_err(store_err)?.ok_or_else(|| {
        mcp_err(
            "workon::annotations::not_found",
            format!("no annotation with uid '{uid}'"),
        )
    })
}

/// Read `path`'s lines for `changeset`: the worktree for the uncommitted layer, or the
/// blob at `changeset`'s branch tip for a committed one. No trailing empty line for a
/// file ending in `\n` (the common case).
fn read_lines(
    repo: &git2::Repository,
    changeset: &ChangesetKey,
    path: &str,
) -> Result<Vec<String>, McpError> {
    let content = if changeset.uncommitted() {
        let workdir = repo.workdir().ok_or_else(|| {
            mcp_err(
                "workon::annotations::mcp::no_workdir",
                "repository has no working directory (bare repo)",
            )
        })?;
        std::fs::read_to_string(workdir.join(path)).map_err(|source| {
            mcp_err(
                "workon::annotations::mcp::content_read_failed",
                format!("reading '{path}' from the worktree: {source}"),
            )
        })?
    } else {
        let object = repo.revparse_single(changeset.name()).map_err(|source| {
            mcp_err(
                "workon::annotations::mcp::revparse_failed",
                format!("resolving changeset '{}': {source}", changeset.name()),
            )
        })?;
        let commit = object.peel_to_commit().map_err(|source| {
            mcp_err(
                "workon::annotations::mcp::revparse_failed",
                format!("'{}' does not name a commit: {source}", changeset.name()),
            )
        })?;
        let tree = commit
            .tree()
            .map_err(|source| mcp_err("workon::annotations::mcp::revparse_failed", source))?;
        let entry = tree.get_path(Path::new(path)).map_err(|source| {
            mcp_err(
                "workon::annotations::mcp::path_not_found",
                format!(
                    "'{path}' not found in '{}'s tree: {source}",
                    changeset.name()
                ),
            )
        })?;
        let blob = repo.find_blob(entry.id()).map_err(|source| {
            mcp_err(
                "workon::annotations::mcp::content_read_failed",
                format!("reading blob for '{path}': {source}"),
            )
        })?;
        String::from_utf8_lossy(blob.content()).into_owned()
    };
    Ok(split_lines(&content))
}

fn split_lines(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Build an anchor for `line` (1-based) against `lines`, capturing up to 3 lines of
/// context each way.
fn build_anchor(
    lines: &[String],
    path: &str,
    new_side: bool,
    line: u32,
) -> Result<Anchor, McpError> {
    if line == 0 {
        return Err(mcp_err(
            "workon::annotations::mcp::invalid_line",
            "line numbers are 1-based",
        ));
    }
    let idx = (line - 1) as usize;
    let target = lines
        .get(idx)
        .ok_or_else(|| {
            mcp_err(
                "workon::annotations::mcp::line_out_of_range",
                format!(
                    "line {line} is out of range ({} lines in '{path}')",
                    lines.len()
                ),
            )
        })?
        .clone();
    let before = lines[idx.saturating_sub(3)..idx].to_vec();
    let after_end = (idx + 1 + 3).min(lines.len());
    let after = lines[idx + 1..after_end].to_vec();

    Ok(Anchor {
        path: path.to_string(),
        new_side,
        lineno: line,
        end_lineno: line,
        target,
        before,
        after,
    })
}

fn parse_side(side: &str) -> Result<bool, McpError> {
    match side {
        "new" => Ok(true),
        "old" => Ok(false),
        other => Err(mcp_err(
            "workon::annotations::mcp::invalid_side",
            format!("side must be \"new\" or \"old\", got \"{other}\""),
        )),
    }
}

fn parse_kind(kind: Option<&str>) -> Result<AnnotationKind, McpError> {
    match kind.unwrap_or("comment") {
        "comment" => Ok(AnnotationKind::Comment),
        "tour_stop" => Ok(AnnotationKind::TourStop),
        "chapter" => Ok(AnnotationKind::Chapter),
        other => Err(mcp_err(
            "workon::annotations::mcp::invalid_kind",
            format!("kind must be \"comment\", \"tour_stop\", or \"chapter\", got \"{other}\""),
        )),
    }
}

// --- Error and JSON plumbing ------------------------------------------------------------

/// Wrap a store error, carrying its `workon::annotations::*` diagnostic code (ADR-021
/// style) into the tool error message rather than losing it to a generic string.
fn store_err(err: workon_annotations::AnnotationsError) -> McpError {
    use miette::Diagnostic;
    let code = err
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "workon::annotations::unknown".to_string());
    McpError::internal_error(format!("{code}: {err}"), None)
}

/// Build a tool error carrying an explicit `workon::annotations::mcp::*` code — this
/// binary's own errors (repo discovery, content capture, bad arguments) follow the same
/// code-prefixed-message convention as the store's, even though they aren't
/// `AnnotationsError` values.
fn mcp_err(code: &str, message: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(format!("{code}: {message}"), None)
}

fn to_json_string<T: serde::Serialize>(value: &T) -> Result<String, McpError> {
    serde_json::to_string_pretty(value)
        .map_err(|source| mcp_err("workon::annotations::mcp::encode_failed", source))
}

fn annotation_to_json(
    annotation: &workon_annotations::Annotation,
    resolution: Option<workon_annotations::anchor::Resolution>,
) -> Value {
    let anchor = annotation.anchor.as_ref().map(|anchor| {
        json!({
            "path": anchor.path,
            "side": if anchor.new_side { "new" } else { "old" },
            "lineno": anchor.lineno,
            "endLineno": anchor.end_lineno,
            "target": anchor.target,
            "before": anchor.before,
            "after": anchor.after,
        })
    });

    let mut value = json!({
        "uid": annotation.uid,
        "kind": kind_str(annotation.kind),
        "status": status_str(annotation.status),
        "parentUid": annotation.parent_uid,
        "changeset": {
            "name": annotation.changeset.name(),
            "uncommitted": annotation.changeset.uncommitted(),
        },
        "anchor": anchor,
        "body": annotation.body,
        "author": annotation.author,
        "tour": annotation.tour,
        "seq": annotation.seq,
        "createdAt": annotation.created_at,
        "updatedAt": annotation.updated_at,
    });

    if let Some(resolution) = resolution {
        value["anchoring"] = json!(anchoring_str(resolution.anchoring));
        value["resolvedLineno"] = json!(resolution.lineno);
    }

    value
}

fn kind_str(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Comment => "comment",
        AnnotationKind::TourStop => "tour_stop",
        AnnotationKind::Chapter => "chapter",
    }
}

fn status_str(status: Status) -> &'static str {
    match status {
        Status::Open => "open",
        Status::Resolved => "resolved",
    }
}

fn anchoring_str(anchoring: workon_annotations::Anchoring) -> String {
    match anchoring {
        workon_annotations::Anchoring::Exact => "exact".to_string(),
        workon_annotations::Anchoring::Shifted { from } => format!("shifted(from={from})"),
        workon_annotations::Anchoring::Orphaned => "orphaned".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards rmcp's silent-empty-router issue (upstream #1174): if `#[tool_router]`
    /// silently drops every route, this catches it before the server ships with zero
    /// tools.
    #[test]
    fn tool_router_serves_exactly_eight_tools() {
        let router = WorkonServer::tool_router();
        assert_eq!(router.list_all().len(), 8);
    }
}
