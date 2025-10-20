use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init_logging() {
    let silent = std::env::var("MCP_SILENT").ok().as_deref() == Some("1")
        || std::env::var("MCP_LOG")
            .ok()
            .map(|v| v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("0"))
            .unwrap_or(false);

    let env_filter = if silent {
        EnvFilter::new("off")
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,cursor_mcp_subagents=info"))
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_thread_ids(false)
        .with_thread_names(false);

    if silent {
        tracing_subscriber::registry().with(env_filter).init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();
    }
}
