use std::process::Command;

pub fn check_cursor_agent(binary: Option<&str>) -> bool {
    let bin = match binary {
        Some(b) => b.to_string(),
        None => which::which("cursor-agent")
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    };
    if bin.is_empty() {
        return false;
    }
    Command::new(bin).arg("--help").output().is_ok()
}

pub fn check_ollama(host: &str) -> bool {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    reqwest::blocking::Client::new()
        .get(url)
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

pub fn check_llama_cpp_cli() -> bool {
    let cli = std::env::var("LLAMA_CPP_CLI").unwrap_or_else(|_| "llama-cli".to_string());
    Command::new(cli).arg("-h").output().is_ok()
}
