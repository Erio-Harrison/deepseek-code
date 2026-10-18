//! Drives the compiled binary as a scripted host. Tests that hit the live API
//! are `#[ignore]`d: run them with `cargo test -- --ignored`.

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

fn spawn_binary() -> Child {
    Command::new(env!("CARGO_BIN_EXE_deepseek-code"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn deepseek-code binary")
}

/// The host keeps stdin open for the whole session; an API failure must still
/// end the process. An unreachable proxy fails the first request offline.
#[tokio::test]
async fn api_failure_exits_with_an_error_while_stdin_is_still_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_deepseek-code"))
        .env("DEEPSEEK_API_KEY", "not-a-real-key")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn deepseek-code binary");
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n")
        .await
        .unwrap();

    let output = timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("the process hung after the API request failed")
        .unwrap();
    drop(stdin);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("deepseek-code:"), "{stderr}");
}

async fn read_until<R>(
    reader: &mut R,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value
where
    R: AsyncBufReadExt + Unpin,
{
    loop {
        let mut line = String::new();
        let n = timeout(Duration::from_secs(30), reader.read_line(&mut line))
            .await
            .expect("timed out waiting for a line from the agent")
            .expect("failed to read line");
        assert!(
            n > 0,
            "agent closed stdout before producing the expected message"
        );
        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("agent emitted invalid JSON line");
        if predicate(&value) {
            return value;
        }
    }
}

#[tokio::test]
#[ignore = "spawns the real binary and hits the live DeepSeek API; run with `cargo test -- --ignored`"]
async fn plain_chat_turn_reaches_end_turn() {
    let mut child = spawn_binary();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    stdin
        .write_all(
            br#"{"type":"user","message":{"role":"user","content":"Say the single word: pong"}}"#,
        )
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();

    let msg = read_until(&mut stdout, |v| v["type"] == "assistant").await;
    assert_eq!(msg["message"]["stop_reason"], "end_turn");

    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "spawns the real binary and hits the live DeepSeek API; run with `cargo test -- --ignored`"]
async fn tool_call_allowed_flows_through_to_tool_result() {
    let mut child = spawn_binary();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let prompt = r#"{"type":"user","message":{"role":"user","content":"Use the bash tool to run: echo hello-from-agent. Then tell me what it printed."}}"#.to_string();
    stdin.write_all(prompt.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();

    let control_request = read_until(&mut stdout, |v| v["type"] == "control_request").await;
    let request_id = control_request["request_id"].as_str().unwrap().to_string();
    assert_eq!(control_request["tool_name"], "bash");

    let response = format!(
        r#"{{"type":"control_response","request_id":"{request_id}","subtype":"success","decision":{{"behavior":"allow"}}}}"#
    );
    stdin.write_all(response.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();

    let tool_result = read_until(&mut stdout, |v| v["type"] == "tool_result").await;
    assert_eq!(tool_result["is_error"], false);
    assert!(
        tool_result["content"]
            .as_str()
            .unwrap()
            .contains("hello-from-agent")
    );

    let final_msg = read_until(&mut stdout, |v| {
        v["type"] == "assistant" && v["message"]["stop_reason"] == "end_turn"
    })
    .await;
    assert!(final_msg["message"]["content"][0]["text"].is_string());

    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "spawns the real binary and hits the live DeepSeek API; run with `cargo test -- --ignored`"]
async fn tool_call_denied_does_not_crash_the_agent() {
    let mut child = spawn_binary();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let prompt = r#"{"type":"user","message":{"role":"user","content":"Use the bash tool to run: echo should-be-denied"}}"#;
    stdin.write_all(prompt.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();

    let control_request = read_until(&mut stdout, |v| v["type"] == "control_request").await;
    let request_id = control_request["request_id"].as_str().unwrap().to_string();

    let response = format!(
        r#"{{"type":"control_response","request_id":"{request_id}","subtype":"success","decision":{{"behavior":"deny","message":"not allowed in this test"}}}}"#
    );
    stdin.write_all(response.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();

    let tool_result = read_until(&mut stdout, |v| v["type"] == "tool_result").await;
    assert_eq!(tool_result["is_error"], true);

    // The agent must still produce a final answer instead of crashing.
    let _final_msg = read_until(&mut stdout, |v| {
        v["type"] == "assistant" && v["message"]["stop_reason"] == "end_turn"
    })
    .await;

    drop(stdin); // EOF: agent's run() loop should return Ok(()) and the process should exit 0
    let status = child.wait().await.unwrap();
    assert!(status.success());
}
