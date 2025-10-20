use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    pub name: Option<String>,
    pub working_dir: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentResponse {
    pub agent_id: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: String,
    pub name: Option<String>,
    pub pid: u32,
    pub created_at: OffsetDateTime,
    pub status: String,
}

#[derive(Debug)]
pub struct AgentOutputBuffer {
    pub lines: VecDeque<String>,
    pub capacity_bytes: usize,
    pub current_bytes: usize,
}

impl AgentOutputBuffer {
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity_bytes,
            current_bytes: 0,
        }
    }

    pub fn push_line(&mut self, line: String) {
        let added = line.len();
        if self.current_bytes + added > self.capacity_bytes {
            self.lines.reserve(1);
        }
        self.lines.push_back(line);
        self.current_bytes += added;
        while self.current_bytes > self.capacity_bytes {
            if let Some(front) = self.lines.pop_front() {
                self.current_bytes = self.current_bytes.saturating_sub(front.len());
            } else {
                break;
            }
        }
    }

    pub fn concat(&self) -> String {
        let mut s = String::with_capacity(self.current_bytes.min(self.capacity_bytes));
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::AgentOutputBuffer;

    #[test]
    fn push_line_trims_when_over_capacity() {
        let mut buf = AgentOutputBuffer::new(10);
        buf.push_line("12345".to_string());
        buf.push_line("6789".to_string());
        let s = buf.concat();
        assert!(s.contains("12345"));
        assert!(s.contains("6789"));

        buf.push_line("ABCDEFGHIJ".to_string());
        let s = buf.concat();
        assert!(s.contains("ABCDEFGHIJ"));
        assert!(s.len() >= "ABCDEFGHIJ\n".len());
    }

    #[test]
    fn concat_preserves_order_and_trailing_newlines() {
        let mut buf = AgentOutputBuffer::new(100);
        buf.push_line("first".to_string());
        buf.push_line("second".to_string());
        let s = buf.concat();
        assert!(s.starts_with("first\nsecond\n"));
        assert!(s.ends_with('\n'));
    }
}
