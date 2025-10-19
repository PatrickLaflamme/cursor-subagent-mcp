use crate::agents::manager::{AgentManagerImpl, StopSignal};
use crate::agents::model::{CreateAgentRequest};
use crate::summarize::{Summarizer};
use serde::Deserialize;
use crate::health;
use serde_json::json;
use std::io::Write;
use std::sync::Arc;

pub struct StdioMcpServer {
    manager: Arc<AgentManagerImpl>,
    summarizer: Arc<dyn Summarizer>,
}

impl StdioMcpServer {
    pub fn new(manager: Arc<AgentManagerImpl>, summarizer: Arc<dyn Summarizer>) -> Self {
        Self { manager, summarizer }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut reader = std::io::BufReader::new(stdin.lock());
        let mut writer = std::io::BufWriter::new(stdout.lock());
        loop {
            let msg = match read_framed_message_buf(&mut reader) {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(error=?e, "stdin closed or invalid frame");
                    break;
                }
            };
            let req: serde_json::Value = match serde_json::from_slice(&msg) {
                Ok(v) => v,
                Err(e) => { tracing::warn!(error=?e, "invalid JSON"); continue; }
            };

            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let id = req.get("id").cloned().unwrap_or(json!(null));
            match method {
                "initialize" => {
                    let result = json!({
                        "capabilities": {
                            "tools": {"list": true, "call": true}
                        },
                        "serverInfo": {"name": "cursor-mcp-subagents", "version": env!("CARGO_PKG_VERSION")}
                    });
                    write_response(&mut writer, id, result)?;
                }
                "tools/list" => {
                    let tools = list_tools_schema();
                    write_response(&mut writer, id, json!({"tools": tools}))?;
                }
                "tools/call" => {
                    let params = req.get("params").cloned().unwrap_or(json!({}));
                    let name = params.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                    let result = self.dispatch_tool(name, arguments).await;
                    match result {
                        Ok(v) => write_response(&mut writer, id, json!({"content": [{"type":"json","json": v}], "isError": false}))?,
                        Err(e) => write_error(&mut writer, id, -32001, &format!("{}", e))?,
                    }
                }
                _ => {
                    write_error(&mut writer, id, -32601, "method not found")?;
                }
            }
        }
        Ok(())
    }

    async fn dispatch_tool(&self, name: &str, arguments: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        match name {
            "create_agent" => {
                let req: CreateAgentRequestWire = serde_json::from_value(arguments)?;
                let resp = self.manager.create(CreateAgentRequest{
                    name: req.name,
                    working_dir: req.working_dir,
                    env: req.env.unwrap_or_default(),
                    args: req.args.unwrap_or_default(),
                }).await?;
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
                }).await??;
                Ok(json!({
                    "summary": res.summary,
                    "tokens_used": res.tokens_used,
                    "backend": res.backend
                }))
            }
            "reset_agent" => {
                let p: ResetAgent = serde_json::from_value(arguments)?;
                self.manager.reset(&p.agent_id, p.hard.unwrap_or(false)).await?;
                Ok(json!({"reset": if p.hard.unwrap_or(false) {"hard"} else {"soft"}}))
            }
            "stop_agent" => {
                let p: StopAgent = serde_json::from_value(arguments)?;
                let signal = match p.signal.as_deref() { Some("kill") => StopSignal::Kill, _ => StopSignal::Term };
                self.manager.stop(&p.agent_id, signal).await?;
                Ok(json!({"stopped": true}))
            }
            "list_agents" => {
                let list = self.manager.list().await;
                Ok(json!({"agents": list}))
            }
            "metrics" => {
                let snap = self.manager.metrics_snapshot();
                Ok(serde_json::to_value(snap)?)
            }
            "health_check" => {
                let bin = self.manager.resolve_binary_path().ok();
                let cursor_ok = health::check_cursor_agent(bin.as_deref());
                let ollama_host = std::env::var("OLLAMA_HOST").ok().unwrap_or_else(|| "http://127.0.0.1:11434".into());
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
        json!({"name":"metrics","description":"Return server agent metrics","inputSchema": {"type":"object","properties":{}}}),
        json!({"name":"health_check","description":"Health check for external dependencies","inputSchema": {"type":"object","properties":{}}}),
    ]
}

fn read_framed_message_buf<R: std::io::BufRead>(bufreader: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut header = String::new();
    let mut content_length: Option<usize> = None;
    loop {
        header.clear();
        let n = bufreader.read_line(&mut header)?;
        if n == 0 { anyhow::bail!("eof"); }
        let line = header.trim_end_matches(['\r','\n']);
        if line.is_empty() { break; }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            let v = rest.trim();
            content_length = Some(v.parse::<usize>()?);
        }
    }
    let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
    let mut body = vec![0u8; len];
    bufreader.read_exact(&mut body)?;
    Ok(body)
}

fn write_response<W: Write>(writer: &mut W, id: serde_json::Value, result: serde_json::Value) -> anyhow::Result<()> {
    let resp = json!({"jsonrpc":"2.0","id": id, "result": result});
    write_framed(writer, &resp)
}

fn write_error<W: Write>(writer: &mut W, id: serde_json::Value, code: i64, message: &str) -> anyhow::Result<()> {
    let resp = json!({"jsonrpc":"2.0","id": id, "error": {"code": code, "message": message}});
    write_framed(writer, &resp)
}

fn write_framed<W: Write>(writer: &mut W, v: &serde_json::Value) -> anyhow::Result<()> {
    let s = serde_json::to_string(v)?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", s.len(), s)?;
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
        fn summarize(&self, context: &str, _instructions: Option<&str>, max_tokens: usize) -> Result<crate::summarize::SummarizeResult, crate::errors::SummarizeError> {
            Ok(crate::summarize::SummarizeResult { summary: context.chars().take(8).collect(), tokens_used: max_tokens.min(1000), backend: "dummy".into() })
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
        let manager = Arc::new(crate::agents::manager::AgentManagerImpl::new(Some("/bin/cat".into()), 16 * 1024));
        let server = StdioMcpServer::new(manager.clone(), Arc::new(DummySummarizer));
        // create agent
        let resp = server.dispatch_tool("create_agent", serde_json::json!({"name":"t","args":[]})).await.unwrap();
        let id = resp.get("agent_id").and_then(|x| x.as_str()).unwrap().to_string();
        // list
        let list = server.dispatch_tool("list_agents", serde_json::json!({})).await.unwrap();
        assert!(list.get("agents").is_some());
        // metrics
        let _ = server.dispatch_tool("metrics", serde_json::json!({})).await.unwrap();
        // stop
        let _ = server.dispatch_tool("stop_agent", serde_json::json!({"agent_id": id})).await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_get_agent_progress_calls_summarizer() {
        let manager = Arc::new(crate::agents::manager::AgentManagerImpl::new(Some("/bin/cat".into()), 16 * 1024));
        let server = StdioMcpServer::new(manager.clone(), Arc::new(DummySummarizer));
        let resp = server.dispatch_tool("create_agent", serde_json::json!({"args":[]})).await.unwrap();
        let id = resp.get("agent_id").and_then(|x| x.as_str()).unwrap().to_string();
        // feed some output
        manager.send_input(&id, "abcdefg").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let res = server.dispatch_tool("get_agent_progress", serde_json::json!({"agent_id": id, "max_tokens": 12})).await.unwrap();
        assert_eq!(res.get("backend").and_then(|x| x.as_str()).unwrap(), "dummy");
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
struct SendAgentInput { agent_id: String, input: String }

#[derive(Debug, Deserialize)]
struct GetAgentProgress { agent_id: String, instructions: Option<String>, max_tokens: Option<usize> }

#[derive(Debug, Deserialize)]
struct ResetAgent { agent_id: String, hard: Option<bool> }

#[derive(Debug, Deserialize)]
struct StopAgent { agent_id: String, signal: Option<String> }


