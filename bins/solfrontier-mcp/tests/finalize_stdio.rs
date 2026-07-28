use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Map, Value};
use solana_sdk::signature::Signature;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    temp_dir: PathBuf,
    cleaned: bool,
}

impl McpProcess {
    fn spawn() -> Self {
        let temp_dir = unique_temp_dir();
        fs::create_dir(&temp_dir).expect("unique MCP test directory must be created");
        let db_path = temp_dir.join("state.sqlite");

        let mut child = Command::new(env!("CARGO_BIN_EXE_solfrontier-mcp"))
            .arg("--db")
            .arg(&db_path)
            .env_remove("SOLFRONTIER_RPC_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("solfrontier-mcp process must start");
        let stdin = child.stdin.take().expect("child stdin must be piped");
        let stdout = child.stdout.take().expect("child stdout must be piped");

        Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            temp_dir,
            cleaned: false,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("MCP stdin must still be open");
        serde_json::to_writer(&mut *stdin, &message).expect("JSON-RPC request must serialize");
        stdin
            .write_all(b"\n")
            .expect("JSON-RPC request must be written");
        stdin.flush().expect("JSON-RPC request must be flushed");
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.read_response(id)
    }

    fn notify(&mut self, method: &str) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
        }));
    }

    fn read_response(&mut self, expected_id: u64) -> Value {
        loop {
            let mut line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .expect("MCP stdout must be readable");
            assert_ne!(
                bytes, 0,
                "MCP server closed stdout before response id {expected_id}"
            );

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let response: Value =
                serde_json::from_str(trimmed).expect("stdout line must contain only JSON-RPC");
            if response.get("id").and_then(Value::as_u64) == Some(expected_id) {
                assert_eq!(response.get("jsonrpc"), Some(&Value::String("2.0".into())));
                assert!(
                    response.get("error").is_none(),
                    "JSON-RPC request {expected_id} failed: {response}"
                );
                return response;
            }
        }
    }

    fn assert_running(&mut self) {
        assert!(
            self.child
                .try_wait()
                .expect("child state must be readable")
                .is_none(),
            "MCP server exited unexpectedly"
        );
    }

    fn shutdown(&mut self) {
        self.stdin.take();
        let status = self.child.wait().expect("MCP server must be waitable");
        assert!(status.success(), "MCP server exited with {status}");
        remove_temp_dir(&self.temp_dir).expect("MCP temp directory must be removed");
        self.cleaned = true;
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if !self.cleaned {
            let _ = remove_temp_dir(&self.temp_dir);
        }
    }
}

fn unique_temp_dir() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch")
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "solfrontier-finalize-stdio-{}-{timestamp}-{sequence}",
        std::process::id()
    ))
}

fn remove_temp_dir(path: &Path) -> io::Result<()> {
    for attempt in 0..20 {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt < 19 => {
                thread::sleep(Duration::from_millis(25));
                if !path.exists() {
                    return Ok(());
                }
                if error.kind() != io::ErrorKind::PermissionDenied {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("cleanup loop always returns")
}

fn response_result(response: &Value) -> &Value {
    response
        .get("result")
        .expect("successful JSON-RPC response must have result")
}

fn tool_text(response: &Value) -> Value {
    let text = response_result(response)
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .expect("tool response must contain text content");
    serde_json::from_str(text).expect("tool text must contain JSON")
}

fn proposal_arguments() -> Value {
    json!({
        "action": "deposit",
        "protocol": "solend",
        "asset": "USDC",
        "display_source": "save",
        "comparison": "gt",
        "amount": "0.5",
        "threshold_bps": 50,
        "expiry_seconds_after_finalize": 180,
        "controlled_wallet": "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L",
        "controlled_usdc_ata": "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3",
        "original_user_message": "If Save APY > 0.5%, deposit 0.5 USDC"
    })
}

#[test]
fn stdio_write_tools_without_rpc_are_fail_closed_and_server_stays_alive() {
    let mut mcp = McpProcess::spawn();

    let initialize = mcp.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {
                "name": "solfrontier-offline-integration-test",
                "version": "1.0.0"
            }
        }),
    );
    assert_eq!(
        response_result(&initialize)
            .pointer("/serverInfo/name")
            .and_then(Value::as_str),
        Some("solfrontier-mcp")
    );
    mcp.notify("notifications/initialized");

    let tools_response = mcp.request(2, "tools/list", json!({}));
    let tools = response_result(&tools_response)
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools/list must return tools");
    let by_name: BTreeMap<&str, &Value> = tools
        .iter()
        .map(|tool| {
            (
                tool.get("name")
                    .and_then(Value::as_str)
                    .expect("tool name must be a string"),
                tool,
            )
        })
        .collect();
    assert_eq!(
        by_name.keys().copied().collect::<Vec<_>>(),
        vec![
            "confirm_funding",
            "finalize_intent",
            "get_intent_status",
            "get_position",
            "get_quote",
            "propose_intent",
        ]
    );
    assert!(
        by_name["finalize_intent"]
            .get("description")
            .and_then(Value::as_str)
            .expect("finalize_intent must have a description")
            .starts_with("WRITE DATABASE:"),
        "finalize_intent must be visibly labeled as a database write"
    );
    assert!(
        by_name["confirm_funding"]
            .get("description")
            .and_then(Value::as_str)
            .expect("confirm_funding must have a description")
            .starts_with("WRITE DATABASE:"),
        "confirm_funding must be visibly labeled as a database write"
    );

    let proposal = proposal_arguments();
    let propose_response = mcp.request(
        3,
        "tools/call",
        json!({
            "name": "propose_intent",
            "arguments": proposal.clone(),
        }),
    );
    let proposed = tool_text(&propose_response);
    assert_eq!(proposed.get("status").and_then(Value::as_str), Some("ok"));

    let mut finalize_arguments: Map<String, Value> = proposal
        .as_object()
        .expect("proposal fixture must be an object")
        .clone();
    finalize_arguments.insert(
        "draft_id".into(),
        proposed
            .get("draft_id")
            .expect("proposal must return draft_id")
            .clone(),
    );
    finalize_arguments.insert(
        "draft_hash".into(),
        proposed
            .get("draft_hash")
            .expect("proposal must return draft_hash")
            .clone(),
    );
    finalize_arguments.insert(
        "user_wallet".into(),
        Value::String("C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW".into()),
    );
    let finalize_response = mcp.request(
        4,
        "tools/call",
        json!({
            "name": "finalize_intent",
            "arguments": finalize_arguments,
        }),
    );
    let finalized = tool_text(&finalize_response);
    assert_eq!(
        finalized.get("status").and_then(Value::as_str),
        Some("config_missing")
    );
    assert_eq!(
        finalized.get("draft_consumed").and_then(Value::as_bool),
        Some(false)
    );

    let confirm_response = mcp.request(
        5,
        "tools/call",
        json!({
            "name": "confirm_funding",
            "arguments": {
                "intent_id": "00000000000000000000000000000000",
                "tx_signature": Signature::from([7_u8; 64]).to_string()
            },
        }),
    );
    let confirmed = tool_text(&confirm_response);
    assert_eq!(
        confirmed.get("status").and_then(Value::as_str),
        Some("not_found")
    );

    let second_list = mcp.request(6, "tools/list", json!({}));
    assert_eq!(
        response_result(&second_list)
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(6)
    );
    mcp.assert_running();
    mcp.shutdown();
}
