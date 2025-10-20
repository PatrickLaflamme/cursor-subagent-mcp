use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "cursor-mcp-subagents")]
#[command(about = "MCP server that manages cursor-agent child processes", long_about = None)]
pub struct AppConfig {
    #[arg(long, env = "CURSOR_AGENT_PATH")]
    pub cursor_agent_path: Option<String>,

    #[arg(long, env = "SUMMARY_BACKEND", default_value = "ollama")]
    pub summary_backend: String,

    #[arg(long, env = "SUMMARY_MODEL", default_value = "llama3.2:3b-instruct")]
    pub summary_model: String,

    #[arg(long, env = "OLLAMA_HOST", default_value = "http://127.0.0.1:11434")]
    pub ollama_host: String,

    #[arg(long, env = "BUFFER_BYTES", default_value_t = 512 * 1024)]
    pub buffer_bytes: u32,

    #[arg(long, env = "IDLE_REAP_MINS")]
    pub idle_reap_mins: Option<u32>,
}

impl AppConfig {
    pub fn from_env_and_args() -> Self {
        Self::parse()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.buffer_bytes == 0 {
            return Err("buffer_bytes must be > 0".into());
        }
        if self.buffer_bytes > 100 * 1024 * 1024 {
            return Err("buffer_bytes too large (max 100MB)".into());
        }
        if self.summary_backend == "ollama" {
            url::Url::parse(&self.ollama_host)
                .map_err(|_| "Invalid OLLAMA_HOST URL format".to_string())?;
        }
        if self.summary_backend == "llama_cpp" {
            if !std::path::Path::new(&self.summary_model).exists() {
                return Err("llama.cpp model file does not exist".into());
            }
        }
        Ok(())
    }
}
