//! Model Context Protocol feature.
//!
//! MCP transport, tools, resources, and protocol adapters belong in this module.

#[path = "impls/server.rs"]
mod server;
#[path = "impls/tools.rs"]
mod tools;

#[path = "impls/http.rs"]
mod http;
#[path = "impls/oauth.rs"]
mod oauth;

pub(crate) fn run(args: &[String]) -> anyhow::Result<()> {
    if let Some(index) = args.iter().position(|arg| arg == "--http-config") {
        anyhow::ensure!(index + 2 == args.len(), "--http-config requires one config path and must be last");
        anyhow::ensure!(args[3..index].iter().all(|arg| arg == "--allow-config-import"), "unknown MCP argument");
        http::run(&args[index + 1], args.iter().any(|arg| arg == "--allow-config-import"))
    } else {
        anyhow::ensure!(args[3..].iter().all(|arg| arg == "--allow-config-import"), "unknown MCP argument; use --http-config <path> for authenticated HTTP");
        server::run_stdio()
    }
}
