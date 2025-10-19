use crate::errors::SummarizeError;

#[derive(Debug, Clone)]
pub struct SummarizeResult {
    pub summary: String,
    pub tokens_used: usize,
    pub backend: String,
}

pub trait Summarizer: Send + Sync {
    fn summarize(&self, context: &str, instructions: Option<&str>, max_tokens: usize) -> Result<SummarizeResult, SummarizeError>;
}


mod ollama;
mod extractive;
#[cfg(feature = "summarizer-llama-cpp")]
mod llama_cpp;
mod cursor_agent;

use std::sync::Arc;

pub fn build_summarizer(backend: String, model: String, ollama_host: String) -> Arc<dyn Summarizer> {
    match backend.as_str() {
        "ollama" => Arc::new(ollama::OllamaSummarizer::new(ollama_host, model)),
        #[cfg(feature = "summarizer-llama-cpp")]
        "llama_cpp" => Arc::new(llama_cpp::LlamaCppSummarizer::new(model)),
        "cursor_agent" => Arc::new(cursor_agent::CursorAgentSummarizer::new(model)),
        _ => Arc::new(extractive::ExtractiveSummarizer::default()),
    }
}


