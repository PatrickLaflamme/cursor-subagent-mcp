use crate::errors::SummarizeError;
use crate::summarize::{SummarizeResult, Summarizer};
use std::process::{Command, Stdio};

pub struct CursorAgentSummarizer {
    model: String,
    bin: String,
}

impl CursorAgentSummarizer {
    pub fn new(model: String) -> Self {
        let bin = std::env::var("CURSOR_AGENT_PATH")
            .ok()
            .unwrap_or_else(|| "cursor-agent".to_string());
        Self { model, bin }
    }
}

impl Summarizer for CursorAgentSummarizer {
    fn summarize(
        &self,
        context: &str,
        instructions: Option<&str>,
        max_tokens: usize,
    ) -> Result<SummarizeResult, SummarizeError> {
        let max_tokens = max_tokens.min(1000);
        let prompt = format!(
            "[System]\nYou are a concise, factual summarizer. Produce a brief progress report focusing on goals, actions taken, results, blockers, next steps. Avoid speculation. Limit to {max_tokens} tokens.\n\n[User]\nInstructions: {}\nContext:\n{}",
            instructions.unwrap_or("summarize overall progress"),
            context
        );

        // Prefer model from config, default to 'auto'
        let model = if self.model.is_empty() {
            "auto"
        } else {
            &self.model
        };

        // Hypothetical CLI flags: --model, --max-tokens, read prompt from stdin
        let mut child = Command::new(&self.bin)
            .arg("--model")
            .arg(model)
            .arg("--max-tokens")
            .arg(max_tokens.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| SummarizeError::Unavailable)?;

        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(prompt.as_bytes())
                .map_err(|e| SummarizeError::Other(format!("write prompt: {e}")))?;
        }

        let mut out = String::new();
        if let Some(mut s) = child.stdout.take() {
            use std::io::Read as _;
            s.read_to_string(&mut out)
                .map_err(|e| SummarizeError::Other(format!("read stdout: {e}")))?;
        }
        let _ = child.wait();

        if out.trim().is_empty() {
            return Err(SummarizeError::Unavailable);
        }

        // Hard cap by characters (~0.25 tokens/char)
        let approx_tokens_per_char = 0.25f64;
        let max_chars = (max_tokens as f64 / approx_tokens_per_char) as usize;
        if out.len() > max_chars {
            out.truncate(max_chars);
        }

        Ok(SummarizeResult {
            summary: out,
            tokens_used: max_tokens,
            backend: "cursor_agent".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn make_echo_script(contents: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("cursor-agent-mock.sh");
        // Keep dir alive by leaking (tests are short-lived)
        let _ = Box::leak(Box::new(dir));
        let mut f = File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        let mut perm = f.metadata().unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        path
    }

    #[test]
    fn returns_summary_from_mock_binary_and_caps_output() {
        // Script echos stdin, ignores flags
        let script = make_echo_script("#!/bin/sh\ncat -");
        std::env::set_var("CURSOR_AGENT_PATH", &script);
        let s = CursorAgentSummarizer::new("auto".into());
        let res = s.summarize("hello world", None, 4).expect("ok");
        assert_eq!(res.backend, "cursor_agent");
        assert_eq!(res.tokens_used, 4);
        assert!(!res.summary.is_empty());
        assert!(res.summary.len() <= 40); // ~4 tokens => ~16 chars, allow slack
    }
}
