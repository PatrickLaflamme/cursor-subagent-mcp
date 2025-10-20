use assert_cmd::prelude::*;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn write_framed<W: Write>(w: &mut W, v: &serde_json::Value) {
    let s = serde_json::to_string(v).unwrap();
    write!(w, "Content-Length: {}\r\n\r\n{}", s.len(), s).unwrap();
    w.flush().unwrap();
}

fn read_framed<R: Read>(r: &mut R) -> serde_json::Value {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(r);
    let mut header = String::new();
    let mut content_length: Option<usize> = None;
    loop {
        header.clear();
        let n = reader.read_line(&mut header).unwrap();
        assert!(n > 0, "unexpected eof reading header");
        let line = header.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse::<usize>().unwrap());
        }
    }
    let len = content_length.expect("missing Content-Length");
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn mcp_end_to_end() {
    // Cross-platform stand-in to satisfy agent spawn without cursor-agent
    let (bin, args): (String, Vec<String>) = if cfg!(windows) {
        ("cmd.exe".into(), vec!["/C".into(), "more".into()])
    } else {
        ("/bin/cat".into(), vec![])
    };
    let mut cmd = Command::cargo_bin("cursor-mcp-subagents").unwrap();
    let mut child = cmd
        .env("CURSOR_AGENT_PATH", bin)
        .env("SUMMARY_BACKEND", "extractive")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    // initialize
    write_framed(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let resp = read_framed(&mut stdout);
    assert_eq!(resp.get("id").and_then(|x| x.as_i64()), Some(1));
    assert!(resp.get("result").is_some());

    // tools/list
    write_framed(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let resp = read_framed(&mut stdout);
    assert_eq!(resp.get("id").and_then(|x| x.as_i64()), Some(2));
    let tools = resp.get("result").and_then(|r| r.get("tools")).unwrap();
    assert!(tools.is_array());

    // create_agent
    write_framed(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params": {"name":"create_agent","arguments":{"name":"t","args": args}}
        }),
    );
    let resp = read_framed(&mut stdout);
    let agent_id = resp
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|j| j.get("json"))
        .and_then(|j| j.get("agent_id"))
        .and_then(|x| x.as_str())
        .unwrap()
        .to_string();

    // send_agent_input
    write_framed(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params": {"name":"send_agent_input","arguments":{"agent_id": agent_id, "input":"hello"}}
        }),
    );
    let _ = read_framed(&mut stdout);

    // give pump time
    std::thread::sleep(Duration::from_millis(120));

    // get_agent_progress (extractive)
    write_framed(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params": {"name":"get_agent_progress","arguments":{"agent_id": agent_id.clone(), "max_tokens": 64}}
        }),
    );
    let resp = read_framed(&mut stdout);
    let content = resp
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|j| j.get("json"))
        .unwrap();
    assert_eq!(
        content.get("backend").and_then(|x| x.as_str()).unwrap(),
        "textrank"
    );
    let summary = content
        .get("summary")
        .and_then(|x| x.as_str())
        .unwrap()
        .to_string();
    assert!(summary.to_lowercase().contains("hello"));

    // stop_agent
    write_framed(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":6,"method":"tools/call",
            "params": {"name":"stop_agent","arguments":{"agent_id": agent_id}}
        }),
    );
    let _ = read_framed(&mut stdout);

    // close stdin to signal server to exit, then best-effort shutdown
    drop(stdin);
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.try_wait();
    let _ = child.kill();
}
