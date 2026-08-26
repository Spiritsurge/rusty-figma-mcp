//! MCP server driving Figma through a plugin over the Host Link Protocol.
//!
//! stdout carries the MCP transport, so every diagnostic goes to stderr (C3).
//! A stray `println!` here corrupts the protocol.

mod tools;

use std::net::IpAddr;
use std::path::Path;

use clap::Parser;
use hostlink::server::{Config, Identity, Server};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::tools::FigmaServer;

#[derive(Parser, Debug)]
#[command(name = "figma-mcp", version, about = "Drive Figma from an MCP client")]
struct Args {
    /// Address to listen on. Off-loopback exposes the open document to the
    /// network and requires a token.
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,

    /// Name shown in the plugin's session list. Defaults to the working
    /// directory's name, which is usually the project you are working on.
    #[arg(long)]
    label: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FIGMA_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // C3: stdout belongs to MCP.
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let label = args.label.unwrap_or_else(default_label);

    // Off-loopback has no user-in-the-UI gesture to lean on (§5), so it gets a
    // token whether or not one was asked for.
    let token = (!args.bind.is_loopback()).then(hostlink::generate_token);

    let server = Server::start(Config {
        identity: Identity::new("figma", &label),
        bind: args.bind,
        token: token.clone(),
        session_dir: None,
    })
    .await?;

    info!(port = server.port, %label, "ready — open the Figma plugin and pick this session");
    if let Some(t) = &token {
        warn!(
            "listening off-loopback; the plugin needs this URL:\n    {}",
            server.connect_url(Some(t))
        );
    }

    let service = FigmaServer::new(server.link.clone()).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// The working directory's name, which is what makes one session
/// distinguishable from another in the plugin's list.
fn default_label() -> String {
    std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "figma-mcp".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_label_is_never_empty() {
        assert!(!default_label().is_empty());
    }

    #[test]
    fn args_default_to_loopback() {
        let args = Args::parse_from(["figma-mcp"]);
        assert!(args.bind.is_loopback());
        assert!(args.label.is_none());
    }

    #[test]
    fn bind_and_label_are_accepted() {
        let args = Args::parse_from(["figma-mcp", "--bind", "0.0.0.0", "--label", "work"]);
        assert!(!args.bind.is_loopback());
        assert_eq!(args.label.as_deref(), Some("work"));
    }
}
