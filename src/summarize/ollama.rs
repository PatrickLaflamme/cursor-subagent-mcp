use crate::errors::SummarizeError;
use crate::summarize::{SummarizeResult, Summarizer};
use reqwest::blocking::Client;

pub struct OllamaSummarizer {
    host: String,
    model: String,
    client: Client,
}

impl OllamaSummarizer {
    pub fn new(host: String, model: String) -> Self {
        Self {
            host,
            model,
            client: Client::new(),
        }
    }
}

impl Summarizer for OllamaSummarizer {
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

        let url = format!("{}/api/generate", self.host.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "options": { "temperature": 0.2, "top_p": 0.9 },
            "stream": false,
            "max_tokens": max_tokens,
        });

        // Simple retry with backoff
        let mut last_err: Option<String> = None;
        for attempt in 0..3 {
            match self.client.post(&url).json(&body).send() {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let v: serde_json::Value = resp
                            .json()
                            .map_err(|e| SummarizeError::Http(format!("decode response: {e}")))?;
                        let summary = v
                            .get("response")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        return Ok(SummarizeResult {
                            summary,
                            tokens_used: max_tokens,
                            backend: "ollama".into(),
                        });
                    } else {
                        last_err = Some(format!("status {} from {}", resp.status(), self.host));
                    }
                }
                Err(e) => {
                    last_err = Some(format!("connect {}: {}", self.host, e));
                }
            }
            // backoff
            std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
        }
        Err(SummarizeError::Http(
            last_err.unwrap_or_else(|| "ollama request failed".into()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn write_http_response(mut stream: TcpStream, status: &str, body: &str) {
        let resp = format!(
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            status,
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }

    #[cfg_attr(windows, ignore)]
    #[test]
    fn success_path_returns_summary() {
        // tiny HTTP server responding with success once
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                // read some of the request to avoid broken pipe
                let mut buf = [0u8; 1024];
                let _ = stream.peek(&mut buf);
                write_http_response(stream, "200 OK", "{\"response\":\"ok summary\"}");
            }
        });

        let host = format!("http://{}:{}", addr.ip(), addr.port());
        let s = OllamaSummarizer::new(host, "test-model".into());
        let res = s
            .summarize("context", Some("instr"), 32)
            .expect("summarize ok");
        assert_eq!(res.backend, "ollama");
        assert_eq!(res.tokens_used, 32);
        assert!(res.summary.contains("ok summary"));
        let _ = handle.join();
    }

    #[test]
    fn retries_and_reports_error_on_failures() {
        // server that returns 500 three times
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut count = 0;
            for _ in 0..3 {
                if let Ok((stream, _)) = listener.accept() {
                    count += 1;
                    let mut buf = [0u8; 1024];
                    let _ = stream.peek(&mut buf);
                    write_http_response(stream, "500 Internal Server Error", "");
                }
            }
            count
        });

        let host = format!("http://{}:{}", addr.ip(), addr.port());
        let s = OllamaSummarizer::new(host, "test-model".into());
        let err = s.summarize("ctx", None, 16).unwrap_err();
        match err {
            SummarizeError::Http(_) => {}
            _ => panic!("expected http error"),
        }
        let _ = handle.join();
    }
}
