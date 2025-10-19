use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,cursor_mcp_subagents=info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(true)
        .with_thread_ids(false)
        .with_thread_names(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();
}


