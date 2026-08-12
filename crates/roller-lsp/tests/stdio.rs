use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn stdio_server_negotiates_and_publishes_diagnostics() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_roller-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let input = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"rootUri": "file:///tmp"},
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {
                "uri": "file:///tmp/build.roller",
                "languageId": "roller",
                "version": 1,
                "text": "section build() { let value = 1 }"
            }},
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ];

    let mut stdin = child.stdin.take().unwrap();
    for message in input {
        let body = serde_json::to_vec(&message).unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stdin.write_all(&body).unwrap();
    }
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "roller-lsp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = decode_messages(&output.stdout);
    assert_eq!(messages[0]["id"], 1);
    assert_eq!(
        messages[0]["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    let diagnostics = messages
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .unwrap();
    assert_eq!(diagnostics["params"]["diagnostics"][0]["code"], "parser");
    assert!(messages.iter().any(|message| message["id"] == 2));
}

fn decode_messages(mut input: &[u8]) -> Vec<Value> {
    let mut messages = Vec::new();
    while !input.is_empty() {
        let header_end = input
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let header = std::str::from_utf8(&input[..header_end]).unwrap();
        let length = header
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let body_start = header_end + 4;
        let body_end = body_start + length;
        messages.push(serde_json::from_slice(&input[body_start..body_end]).unwrap());
        input = &input[body_end..];
    }
    messages
}
