use std::sync::Arc;

mod agents;
mod config;
mod errors;
mod health;
mod logging;
mod mcp;
mod summarize;

use crate::agents::manager::AgentManagerImpl;
use crate::config::AppConfig;
use crate::mcp::StdioMcpServer;
use crate::summarize::build_summarizer;

#[tokio::main]
async fn main() {
    logging::init_logging();

    let cfg = AppConfig::from_env_and_args();

    let summarizer = build_summarizer(
        cfg.summary_backend.clone(),
        cfg.summary_model.clone(),
        cfg.ollama_host.clone(),
    );

    let agent_manager = Arc::new(AgentManagerImpl::new(
        cfg.cursor_agent_path.clone(),
        cfg.buffer_bytes as usize,
    ));

    // Startup health checks (best-effort, logged only)
    let cursor_ok = health::check_cursor_agent(cfg.cursor_agent_path.as_deref());
    let ollama_ok = if cfg.summary_backend == "ollama" {
        health::check_ollama(&cfg.ollama_host)
    } else {
        false
    };
    let llama_ok = if cfg.summary_backend == "llama_cpp" {
        health::check_llama_cpp_cli()
    } else {
        false
    };
    tracing::info!(
        cursor_agent_ok=cursor_ok,
        ollama_ok=ollama_ok,
        llama_cpp_ok=llama_ok,
        summary_backend=%cfg.summary_backend,
        buffer_size=%cfg.buffer_bytes,
        "MCP server startup complete"
    );

    if let Err(e) = cfg.validate() {
        tracing::warn!(config_error=%e, "invalid config");
    }
    let server = StdioMcpServer::new(agent_manager.clone(), summarizer);
    // Graceful shutdown without spawning (run future is not Send due to stdio locks)
    tokio::select! {
        res = server.run() => {
            if let Err(e) = res { tracing::error!(error=?e, "server terminated with error") }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received shutdown signal, stopping all agents...");
            agent_manager.stop_all().await;
        }
    }
}
