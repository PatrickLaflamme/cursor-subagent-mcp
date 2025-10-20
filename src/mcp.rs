use crate::agents::manager::{AgentManagerImpl, StopSignal};
use crate::agents::model::CreateAgentRequest;
use crate::health;
use crate::summarize::Summarizer;
use serde::Deserialize;
use serde_json::json;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// Global switch: once we detect raw JSON (no Content-Length) from the client,
// we reply in ND-JSON (one JSON per line, no headers).
static RAW_JSON_MODE: AtomicBool = AtomicBool::new(false);

pub struct StdioMcpServer {
    manager: Arc<AgentManagerImpl>,
    summarizer: Arc<dyn Summarizer>,
}

impl StdioMcpServer {
    pub fn new(manager: Arc<AgentManagerImpl>, summarizer: Arc<dyn Summarizer>) -> Self {
        Self {
            manager,
            summarizer,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut reader = std::io::BufReader::new(stdin.lock());
        let mut writer = std::io::BufWriter::new(stdout.lock());
        tracing::info!("run loop started: waiting for framed MCP requests on stdin");
        loop {
            tracing::info!("waiting to parse next frame header");
            let msg = match read_framed_message_buf(&mut reader) {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(error=?e, "stdin closed or invalid frame");
                    break;
                }
            };
            let req: serde_json::Value = match serde_json::from_slice(&msg) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error=?e, "invalid JSON");
                    continue;
                }
            };

            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let id_opt = req.get("id").cloned();
            let id_reply = id_opt.as_ref().filter(|v| !v.is_null()).cloned();
            tracing::info!(%method, id=?id_opt, "received request");
            match method {
                "initialize" => {
                    let params = req.get("params").cloned().unwrap_or(json!({}));
                    let client_proto = params
                        .get("protocolVersion")
                        .and_then(|x| x.as_str())
                        .unwrap_or("2024-11-05");
                    let result = json!({
                        "protocolVersion": client_proto,
                        "capabilities": {
                            "tools": {"list": true, "call": true},
                            "prompts": {"list": true},
                            "resources": {"list": true, "read": true, "subscribe": false}
                        },
                        "serverInfo": {"name": "cursor-mcp-subagents", "version": env!("CARGO_PKG_VERSION")}
                    });
                    if let Some(id) = id_reply.clone() {
                        write_response(&mut writer, id, result)?;
                    }
                }
                "server/info" => {
                    let info = json!({"name": "cursor-mcp-subagents", "version": env!("CARGO_PKG_VERSION")});
                    if let Some(id) = id_reply.clone() {
                        write_response(&mut writer, id, json!({"serverInfo": info}))?;
                    }
                }
                "tools/list" => {
                    let tools = list_tools_schema();
                    if let Some(id) = id_reply.clone() {
                        write_response(&mut writer, id, json!({"tools": tools}))?;
                    }
                }
                "prompts/list" => {
                    if let Some(id) = id_reply.clone() {
                        write_response(&mut writer, id, json!({"prompts": []}))?;
                    }
                }
                "resources/list" => {
                    let resources = vec![
                        json!({
                            "uri": "mcp://cursor-mcp-subagents/metrics",
                            "name": "Server metrics snapshot",
                            "description": "Current metrics and counters for the MCP server",
                            "mimeType": "application/json"
                        }),
                        json!({
                            "uri": "mcp://cursor-mcp-subagents/agents",
                            "name": "Active agents list",
                            "description": "List of currently running agents managed by the server",
                            "mimeType": "application/json"
                        }),
                    ];
                    if let Some(id) = id_reply.clone() {
                        write_response(&mut writer, id, json!({"resources": resources}))?;
                    }
                }
                "resources/read" => {
                    let params = req.get("params").cloned().unwrap_or(json!({}));
                    let uri = params.get("uri").and_then(|x| x.as_str()).unwrap_or("");
                    let (mime, text) = match uri {
                        "mcp://cursor-mcp-subagents/metrics" => {
                            let snap = self.manager.metrics_snapshot();
                            (
                                "application/json",
                                serde_json::to_string_pretty(&snap).unwrap_or_else(|_| "{}".into()),
                            )
                        }
                        "mcp://cursor-mcp-subagents/agents" => {
                            let list = self.manager.list().await;
                            (
                                "application/json",
                                serde_json::to_string_pretty(&json!({"agents": list}))
                                    .unwrap_or_else(|_| "{}".into()),
                            )
                        }
                        _ => {
                            if let Some(id) = id_reply.clone() {
                                write_error(&mut writer, id, -32602, "Unknown resource uri")?;
                            }
                            continue;
                        }
                    };
                    let contents = vec![json!({
                        "uri": uri,
                        "mimeType": mime,
                        "text": text
                    })];
                    if let Some(id) = id_reply.clone() {
                        write_response(&mut writer, id, json!({"contents": contents}))?;
                    }
                }
                "tools/call" => {
                    let params = req.get("params").cloned().unwrap_or(json!({}));
                    let name = params.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                    let result = self.dispatch_tool(name, arguments).await;
                    match result {
                        Ok(v) => {
                            if let Some(id) = id_reply.clone() {
                                write_response(
                                    &mut writer,
                                    id,
                                    json!({"content": [{"type":"json","json": v}], "isError": false}),
                                )?;
                            }
                        }
                        Err(e) => {
                            if let Some(id) = id_reply.clone() {
                                write_error(&mut writer, id, -32001, &format!("{}", e))?;
                            }
                        }
                    }
                }
                _ => {
                    // Do not respond to notifications (no id)
                    if let Some(id) = id_reply.clone() {
                        write_error(&mut writer, id, -32601, "method not found")?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn dispatch_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        match name {
            "create_agent" => {
                let req: CreateAgentRequestWire = serde_json::from_value(arguments)?;
                let resp = self
                    .manager
                    .create(CreateAgentRequest {
                        name: req.name,
                        working_dir: req.working_dir,
                        env: req.env.unwrap_or_default(),
                        args: req.args.unwrap_or_default(),
                    })
                    .await?;
                Ok(serde_json::to_value(resp)?)
            }
            "send_agent_input" => {
                let p: SendAgentInput = serde_json::from_value(arguments)?;
                self.manager.send_input(&p.agent_id, &p.input).await?;
                Ok(json!({"accepted": true}))
            }
            "get_agent_progress" => {
                let p: GetAgentProgress = serde_json::from_value(arguments)?;
                let buf = self.manager.get_buffer(&p.agent_id).await?;
                let max_tokens = p.max_tokens.unwrap_or(1000).min(1000);
                let summarizer = self.summarizer.clone();
                let instructions = p.instructions.clone();
                let res = tokio::task::spawn_blocking(move || {
                    summarizer.summarize(&buf, instructions.as_deref(), max_tokens)
                })
                .await??;
                Ok(json!({
                    "summary": res.summary,
                    "tokens_used": res.tokens_used,
                    "backend": res.backend
                }))
            }
            "reset_agent" => {
                let p: ResetAgent = serde_json::from_value(arguments)?;
                self.manager
                    .reset(&p.agent_id, p.hard.unwrap_or(false))
                    .await?;
                Ok(json!({"reset": if p.hard.unwrap_or(false) {"hard"} else {"soft"}}))
            }
            "stop_agent" => {
                let p: StopAgent = serde_json::from_value(arguments)?;
                let signal = match p.signal.as_deref() {
                    Some("kill") => StopSignal::Kill,
                    _ => StopSignal::Term,
                };
                self.manager.stop(&p.agent_id, signal).await?;
                Ok(json!({"stopped": true}))
            }
            "list_agents" => {
                let list = self.manager.list().await;
                Ok(json!({"agents": list}))
            }
            "wait" => {
                let p: WaitParams = serde_json::from_value(arguments)?;
                let ms = if let Some(ms) = p.ms {
                    ms
                } else if let Some(secs) = p.seconds {
                    secs.saturating_mul(1000)
                } else {
                    0
                };
                if ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                }
                Ok(json!({"waited_ms": ms}))
            }
            "metrics" => {
                let snap = self.manager.metrics_snapshot();
                Ok(serde_json::to_value(snap)?)
            }
            "health_check" => {
                let bin = self.manager.resolve_binary_path().ok();
                let cursor_ok = health::check_cursor_agent(bin.as_deref());
                let ollama_host = std::env::var("OLLAMA_HOST")
                    .ok()
                    .unwrap_or_else(|| "http://127.0.0.1:11434".into());
                let ollama_ok = health::check_ollama(&ollama_host);
                let llama_ok = health::check_llama_cpp_cli();
                Ok(json!({
                    "cursor_agent_ok": cursor_ok,
                    "ollama_ok": ollama_ok,
                    "llama_cpp_ok": llama_ok,
                    "server": {"name": "cursor-mcp-subagents", "version": env!("CARGO_PKG_VERSION")}
                }))
            }
            _ => anyhow::bail!("unknown tool: {name}"),
        }
    }
}

fn list_tools_schema() -> Vec<serde_json::Value> {
    vec![
        json!({"name":"create_agent","description":"Create a persistent cursor-agent process","inputSchema":{"type":"object","properties":{
            "name": {"type":"string"},
            "working_dir": {"type":"string"},
            "env": {"type":"object","additionalProperties":{"type":"string"}},
            "args": {"type":"array","items":{"type":"string"}}
        }}}),
        json!({"name":"send_agent_input","description":"Send input line to agent stdin","inputSchema": {"type":"object","required":["agent_id","input"],"properties":{
            "agent_id":{"type":"string"},
            "input":{"type":"string"}
        }}}),
        json!({"name":"get_agent_progress","description":"Summarize agent buffered output","inputSchema": {"type":"object","required":["agent_id"],"properties":{
            "agent_id":{"type":"string"},
            "instructions":{"type":"string"},
            "max_tokens":{"type":"number"}
        }}}),
        json!({"name":"reset_agent","description":"Reset agent buffer or restart process","inputSchema": {"type":"object","required":["agent_id"],"properties":{
            "agent_id":{"type":"string"},
            "hard":{"type":"boolean"}
        }}}),
        json!({"name":"stop_agent","description":"Stop and remove agent","inputSchema": {"type":"object","required":["agent_id"],"properties":{
            "agent_id":{"type":"string"},
            "signal":{"type":"string","enum":["term","kill"]}
        }}}),
        json!({"name":"list_agents","description":"List running agents","inputSchema": {"type":"object","properties":{}}}),
        json!({"name":"wait","description":"Sleep for the specified duration","inputSchema": {"type":"object","properties":{
            "ms": {"type":"number"},
            "seconds": {"type":"number"}
        }}}),
        json!({"name":"metrics","description":"Return server agent metrics","inputSchema": {"type":"object","properties":{}}}),
        json!({"name":"health_check","description":"Health check for external dependencies","inputSchema": {"type":"object","properties":{}}}),
    ]
}

fn read_framed_message_buf<R: std::io::BufRead>(bufreader: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut header = String::new();
    let mut content_length: Option<usize> = None;
    let mut header_lines: usize = 0;
    loop {
        header.clear();
        let n = bufreader.read_line(&mut header)?;
        if n == 0 {
            anyhow::bail!("eof");
        }
        let line = header.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        header_lines += 1;
        tracing::trace!(%line, "framing header line");
        // Fallback for clients that send newline-delimited raw JSON instead of framed headers
        if header_lines == 1 && line.starts_with('{') && line.contains("\"jsonrpc\"") {
            tracing::debug!("detected raw JSON line without Content-Length; accepting as body");
            RAW_JSON_MODE.store(true, Ordering::Relaxed);
            return Ok(line.as_bytes().to_vec());
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("content-length") {
                let v = value.trim();
                content_length = Some(v.parse::<usize>()?);
            }
            // ignore other headers (e.g., Content-Type)
        }
    }
    let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
    let mut body = vec![0u8; len];
    bufreader.read_exact(&mut body)?;
    let preview = std::str::from_utf8(&body)
        .ok()
        .map(|s| if s.len() > 200 { &s[..200] } else { s })
        .unwrap_or("<non-utf8>");
    tracing::trace!(header_lines, content_length=len, body_bytes=body.len(), preview=%preview, "framed message parsed");
    Ok(body)
}

fn write_response<W: Write>(
    writer: &mut W,
    id: serde_json::Value,
    result: serde_json::Value,
) -> anyhow::Result<()> {
    let resp = json!({"jsonrpc":"2.0","id": id, "result": result});
    write_framed(writer, &resp)
}

fn write_error<W: Write>(
    writer: &mut W,
    id: serde_json::Value,
    code: i64,
    message: &str,
) -> anyhow::Result<()> {
    let resp = json!({"jsonrpc":"2.0","id": id, "error": {"code": code, "message": message}});
    write_framed(writer, &resp)
}

fn write_framed<W: Write>(writer: &mut W, v: &serde_json::Value) -> anyhow::Result<()> {
    let s = serde_json::to_string(v)?;
    // Respond in ND-JSON mode if detected (or forced), otherwise use Content-Length framing.
    // This maximizes compatibility with editors that do not send LSP-style headers over stdio.
    let force_ndjson = std::env::var("MCP_FORCE_NDJSON").ok().as_deref() == Some("1");
    if force_ndjson || RAW_JSON_MODE.load(Ordering::Relaxed) {
        writeln!(writer, "{}", s)?;
    } else {
        write!(writer, "Content-Length: {}\r\n\r\n{}", s.len(), s)?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // no extra imports needed here
    use std::sync::Arc;

    struct DummySummarizer;
    impl Summarizer for DummySummarizer {
        fn summarize(
            &self,
            context: &str,
            _instructions: Option<&str>,
            max_tokens: usize,
        ) -> Result<crate::summarize::SummarizeResult, crate::errors::SummarizeError> {
            Ok(crate::summarize::SummarizeResult {
                summary: context.chars().take(8).collect(),
                tokens_used: max_tokens.min(1000),
                backend: "dummy".into(),
            })
        }
    }

    #[test]
    fn framed_write_and_read_roundtrip() {
        let v = serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
        let mut out = Vec::new();
        write_framed(&mut out, &v).expect("write");
        let mut cursor = std::io::Cursor::new(out);
        // read via helper
        let mut bufreader = std::io::BufReader::new(&mut cursor);
        let body = read_framed_message_buf(&mut bufreader).expect("read");
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, v);
    }

    #[tokio::test]
    async fn dispatch_create_list_and_metrics() {
        let manager = Arc::new(crate::agents::manager::AgentManagerImpl::new(
            Some({
                #[cfg(unix)]
                {
                    "/bin/cat".into()
                }
                #[cfg(windows)]
                {
                    "cmd.exe".into()
                }
            }),
            16 * 1024,
        ));
        let server = StdioMcpServer::new(manager.clone(), Arc::new(DummySummarizer));
        // create agent
        let args: Vec<String> = if cfg!(windows) {
            vec!["/C".into(), "more".into()]
        } else {
            Vec::new()
        };
        let resp = server
            .dispatch_tool(
                "create_agent",
                serde_json::json!({
                    "name":"t",
                    "args": args
                }),
            )
            .await
            .unwrap();
        let id = resp
            .get("agent_id")
            .and_then(|x| x.as_str())
            .unwrap()
            .to_string();
        // list
        let list = server
            .dispatch_tool("list_agents", serde_json::json!({}))
            .await
            .unwrap();
        assert!(list.get("agents").is_some());
        // metrics
        let _ = server
            .dispatch_tool("metrics", serde_json::json!({}))
            .await
            .unwrap();
        // stop
        let _ = server
            .dispatch_tool("stop_agent", serde_json::json!({"agent_id": id}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dispatch_get_agent_progress_calls_summarizer() {
        let manager = Arc::new(crate::agents::manager::AgentManagerImpl::new(
            Some({
                #[cfg(unix)]
                {
                    "/bin/cat".into()
                }
                #[cfg(windows)]
                {
                    "cmd.exe".into()
                }
            }),
            16 * 1024,
        ));
        let server = StdioMcpServer::new(manager.clone(), Arc::new(DummySummarizer));
        let args: Vec<String> = if cfg!(windows) {
            vec!["/C".into(), "more".into()]
        } else {
            Vec::new()
        };
        let resp = server
            .dispatch_tool("create_agent", serde_json::json!({ "args": args }))
            .await
            .unwrap();
        let id = resp
            .get("agent_id")
            .and_then(|x| x.as_str())
            .unwrap()
            .to_string();
        // feed some output
        manager.send_input(&id, "abcdefg").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let res = server
            .dispatch_tool(
                "get_agent_progress",
                serde_json::json!({"agent_id": id, "max_tokens": 12}),
            )
            .await
            .unwrap();
        assert_eq!(
            res.get("backend").and_then(|x| x.as_str()).unwrap(),
            "dummy"
        );
        assert_eq!(res.get("tokens_used").and_then(|x| x.as_u64()).unwrap(), 12);
        assert!(res.get("summary").and_then(|x| x.as_str()).unwrap().len() <= 8);
    }
}

// Wire structs for tool params
#[derive(Debug, Deserialize)]
struct CreateAgentRequestWire {
    name: Option<String>,
    working_dir: Option<std::path::PathBuf>,
    env: Option<std::collections::HashMap<String, String>>,
    args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SendAgentInput {
    agent_id: String,
    input: String,
}

#[derive(Debug, Deserialize)]
struct GetAgentProgress {
    agent_id: String,
    instructions: Option<String>,
    max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ResetAgent {
    agent_id: String,
    hard: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct StopAgent {
    agent_id: String,
    signal: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WaitParams {
    ms: Option<u64>,
    seconds: Option<u64>,
}
