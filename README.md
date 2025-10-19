# cursor-mcp-subagents

Lightweight Rust MCP server that manages persistent `cursor-agent` child processes for isolated subtask delegation. Provides async lifecycle controls and local summarization via Ollama (with extractive fallback).

## Features

- Create multiple persistent agents (create_agent), send input, list, reset, and stop
- get_agent_progress returns a concise summary (<=1000 tokens) of buffered agent output
- Summarization backends: Ollama (default), llama.cpp (feature, TODO), extractive fallback
- Async I/O, bounded ring buffers, best-effort stop/reset timeouts

## Install

```bash
cargo build --release
```

## Run as a Cursor MCP server

Add to your Cursor MCP config:

```json
{
  "mcpServers": {
    "cursor-subagents": {
      "command": "/absolute/path/to/target/release/cursor-mcp-subagents",
      "args": [],
      "env": {
        "CURSOR_AGENT_PATH": "/usr/local/bin/cursor-agent",
        "SUMMARY_BACKEND": "ollama",
        "SUMMARY_MODEL": "llama3.2:3b-instruct",
        "OLLAMA_HOST": "http://127.0.0.1:11434",
        "BUFFER_BYTES": "524288"
      }
    }
  }
}
```

## Tools

- create_agent: Create a persistent cursor-agent process
- send_agent_input: Send a line to agent stdin
- get_agent_progress: Summarize buffered agent output (optional instructions)
- reset_agent: Soft (clear buffer) or hard (restart process)
- stop_agent: Gracefully stop; kill on demand
- list_agents: Return metadata for running agents
- metrics: Return server metrics snapshot
- health_check: Live connectivity checks

## Summarization

Recommended Ollama models:
- Default: llama3.2:3b-instruct
- Ultra-light: llama3.2:1b-instruct
- CPU-balanced: qwen2.5:1.5b-instruct

Adjust via SUMMARY_MODEL env var.

To use cursor-agent for summarization (auto model):

```bash
export SUMMARY_BACKEND=cursor_agent
export SUMMARY_MODEL=auto
export CURSOR_AGENT_PATH=/usr/local/bin/cursor-agent
```

To use llama.cpp instead of Ollama, build with the `summarizer-llama-cpp` feature and set:

```bash
export SUMMARY_BACKEND=llama_cpp
export SUMMARY_MODEL=/absolute/path/to/model.gguf
# optionally override CLI path (defaults to llama-cli on PATH)
export LLAMA_CPP_CLI=/usr/local/bin/llama-cli
```

## Notes

- This server communicates via JSON-RPC over stdio (MCP framing)
- Requires cursor-agent in PATH or CURSOR_AGENT_PATH env set

## Troubleshooting

- Ollama connection errors: ensure `OLLAMA_HOST` is reachable; try `curl $OLLAMA_HOST/api/tags`.
- llama.cpp model path invalid: set `SUMMARY_MODEL` to an existing `.gguf` file.
- cursor-agent not found: install it or set `CURSOR_AGENT_PATH`.
- Summaries too long: lower `max_tokens` in `get_agent_progress`.

## License

MIT or Apache-2.0
