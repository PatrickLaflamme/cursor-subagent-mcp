use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("spawn error: {0}")]
    Spawn(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
}

#[derive(Debug, Error)]
pub enum SummarizeError {
    #[error("backend unavailable")]
    Unavailable,
    #[error("http error: {0}")]
    Http(String),
    #[error("other: {0}")]
    Other(String),
}


