use crate::errors::SummarizeError;
use crate::summarize::{SummarizeResult, Summarizer};
use std::io::Read;
use std::process::{Command, Stdio};

pub struct LlamaCppSummarizer {
    model_path: String,
    cli_path: String,
}

impl LlamaCppSummarizer {
    pub fn new(model_path: String) -> Self {
        // Allow overriding CLI path via env; default to "llama-cli" on PATH
        let cli_path = std::env::var("LLAMA_CPP_CLI").unwrap_or_else(|_| "llama-cli".to_string());
        Self {
            model_path,
            cli_path,
        }
    }
}

impl Summarizer for LlamaCppSummarizer {
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

        // Invoke llama.cpp CLI: llama-cli -m <model.gguf> -p <prompt> -n <max_tokens>
        let mut child = Command::new(&self.cli_path)
            .arg("-m")
            .arg(&self.model_path)
            .arg("-p")
            .arg(&prompt)
            .arg("-n")
            .arg(max_tokens.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| SummarizeError::Unavailable)?;

        let mut out = String::new();
        if let Some(mut s) = child.stdout.take() {
            use std::io::Read as _;
            s.read_to_string(&mut out)
                .map_err(|e| SummarizeError::Other(format!("read stdout: {e}")))?;
        }
        // Best-effort: wait but don't block forever
        let _ = child.wait();

        if out.trim().is_empty() {
            return Err(SummarizeError::Unavailable);
        }

        // Enforce a hard character cap approximating token budget
        let approx_tokens_per_char = 0.25f64;
        let max_chars = (max_tokens as f64 / approx_tokens_per_char) as usize;
        if out.len() > max_chars {
            out.truncate(max_chars);
        }

        Ok(SummarizeResult {
            summary: out,
            tokens_used: max_tokens,
            backend: "llama_cpp".into(),
        })
    }
}

#[cfg(all(test, feature = "summarizer-llama-cpp"))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn make_cli_script(output: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("llama-cli-mock.sh");
        let _ = Box::leak(Box::new(dir));
        let mut f = File::create(&path).unwrap();
        // Ignore args and just print output
        let script = format!("#!/bin/sh\necho {}\n", shell_escape::escape(output.into()));
        f.write_all(script.as_bytes()).unwrap();
        let mut perm = f.metadata().unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        path
    }

    #[test]
    fn returns_output_and_respects_char_cap() {
        let script = make_cli_script("ok-llama-output");
        std::env::set_var("LLAMA_CPP_CLI", &script);
        let s = LlamaCppSummarizer::new("model.gguf".into());
        let res = s.summarize("ctx", None, 4).expect("ok");
        assert_eq!(res.backend, "llama_cpp");
        assert_eq!(res.tokens_used, 4);
        assert!(!res.summary.is_empty());
        assert!(res.summary.len() <= 40);
    }
}
