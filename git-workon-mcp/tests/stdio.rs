//! Drives `git-workon-mcp` as a real child process over its stdio transport: the MCP
//! handshake, `tools/list`, and a `tools/call` round-trip (post an annotation, then fetch
//! it back), asserted against the sqlite file the binary wrote to.
//!
//! Newline-delimited JSON-RPC, no `Content-Length` framing (confirmed against rmcp's own
//! `transport::io::stdio` tests) — a raw `std::process::Command` plus `BufRead::read_line`
//! is enough; no async runtime needed on the test side.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use serde_json::{json, Value};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        let bin = env!("CARGO_BIN_EXE_git-workon-mcp");
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn git-workon-mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn send_notification(&mut self, method: &str) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        self.write_line(&message);
    }

    /// Send a request and return its `result` field (panics on a JSON-RPC error response).
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_line(&message);

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read response line");
        assert!(!line.is_empty(), "server closed stdout without a response");
        let response: Value = serde_json::from_str(&line).expect("response is valid JSON");
        assert_eq!(response["id"], json!(id), "response id: {response}");
        if let Some(error) = response.get("error") {
            panic!("{method} returned a JSON-RPC error: {error}");
        }
        response["result"].clone()
    }

    fn write_line(&mut self, message: &Value) {
        let mut line = serde_json::to_string(message).expect("serialize request");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.flush().expect("flush request");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_repo() -> (tempfile::TempDir, git2::Repository) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("init repo");
    std::fs::write(
        dir.path().join("hello.txt"),
        "one\ntwo\nthree\nfour\nfive\n",
    )
    .expect("write fixture file");
    (dir, repo)
}

#[test]
fn initialize_then_list_tools() {
    let mut server = Server::spawn();

    let init = server.call(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp-stdio-test", "version": "0.0.0" },
        }),
    );
    assert_eq!(init["serverInfo"]["name"], json!("git-workon-mcp"));
    server.send_notification("notifications/initialized");

    let tools = server.call("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();

    let mut sorted = names.clone();
    sorted.sort_unstable();
    let mut expected = vec![
        "annotation_list",
        "annotation_get",
        "annotation_post",
        "annotation_reply",
        "annotation_resolve",
        "annotation_update",
        "annotation_delete",
        "walkthrough_put",
    ];
    expected.sort_unstable();
    assert_eq!(sorted, expected, "unexpected tool set: {names:?}");
}

#[test]
fn post_then_get_round_trips_through_the_store() {
    let (dir, _repo) = init_repo();
    let mut server = Server::spawn();

    server.call(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp-stdio-test", "version": "0.0.0" },
        }),
    );
    server.send_notification("notifications/initialized");

    let repo_path = dir.path().to_str().expect("utf8 path").to_string();

    let post_result = server.call(
        "tools/call",
        json!({
            "name": "annotation_post",
            "arguments": {
                "repoPath": repo_path,
                "changeset": "main",
                "uncommitted": true,
                "path": "hello.txt",
                "side": "new",
                "line": 3,
                "body": "why three?",
                "author": "agent",
            },
        }),
    );
    let posted: Value =
        serde_json::from_str(text_content(&post_result)).expect("post result is JSON");
    assert_eq!(posted["body"], json!("why three?"));
    assert_eq!(posted["anchor"]["target"], json!("three"));
    assert_eq!(posted["anchor"]["before"], json!(["one", "two"]));
    assert_eq!(posted["anchor"]["after"], json!(["four", "five"]));
    let uid = posted["uid"].as_str().expect("uid").to_string();

    let get_result = server.call(
        "tools/call",
        json!({
            "name": "annotation_get",
            "arguments": { "repoPath": repo_path, "uid": uid },
        }),
    );
    let fetched: Value =
        serde_json::from_str(text_content(&get_result)).expect("get result is JSON");
    assert_eq!(fetched["uid"], json!(uid));
    assert_eq!(fetched["author"], json!("agent"));
    assert_eq!(fetched["status"], json!("open"));
}

/// Pull the text out of a `tools/call` result's `content` array — rmcp wraps a `String`
/// tool return as a single text content block.
fn text_content(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected text content in {result}"))
}
