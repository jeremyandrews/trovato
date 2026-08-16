//! Trovato MCP Server
//!
//! A Model Context Protocol (MCP) server that exposes Trovato CMS content
//! and schema to external AI tools (Claude Desktop, Cursor, VS Code).
//!
//! Runs as a STDIO transport server, authenticated via API token.
//!
//! # Usage
//!
//! Preferred (token not visible in process list):
//! ```sh
//! TROVATO_API_TOKEN=trv_abc123... trovato-mcp
//! ```
//!
//! Or read from a file:
//! ```sh
//! trovato-mcp --token-file /path/to/token
//! ```
//!
//! Or pass directly (WARNING: token visible in `ps` output):
//! ```sh
//! trovato-mcp --token trv_abc123...
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use trovato_kernel::Config;
use trovato_kernel::state::AppState;

use trovato_mcp::auth;
use trovato_mcp::server::TrovatoMcpServer;

/// Trovato MCP Server — expose CMS content to AI tools via MCP.
#[derive(Parser)]
#[command(name = "trovato-mcp", about = "Trovato CMS MCP server")]
struct Cli {
    /// API token for authentication (WARNING: visible in process listing).
    ///
    /// Prefer TROVATO_API_TOKEN env var or --token-file instead.
    /// Create tokens at /user/{id}/tokens in the Trovato admin UI.
    #[arg(long, env = "TROVATO_API_TOKEN")]
    token: Option<String>,

    /// Path to a file containing the API token (one line, trimmed).
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing to stderr (stdout is the MCP transport)
    init_tracing();

    let cli = Cli::parse();

    // Resolve the API token from --token, --token-file, or env
    let raw_token = resolve_cli_token(&cli)?;

    // Load kernel configuration from environment
    let config = Config::from_env().context("failed to load configuration")?;
    tracing::info!("Initializing Trovato MCP server");

    // Initialize full kernel state (DB pool, Redis, services)
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;

    tracing::info!("Kernel state initialized");

    // Resolve API token to user
    let user = auth::resolve_token(&state, &raw_token)
        .await
        .context("failed to authenticate API token")?;

    // Build user context with pre-loaded permissions for service-layer calls
    let user_ctx = auth::build_user_context(&state, &user)
        .await
        .context("failed to build user context")?;

    tracing::info!(
        user_id = %user.id,
        username = %user.name,
        is_admin = user.is_admin,
        permissions = user_ctx.permissions.len(),
        "Authenticated MCP user"
    );

    // Create and run the MCP server on STDIO transport
    let server = TrovatoMcpServer::new(state, raw_token, user_ctx);

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("failed to start MCP server")?;

    tracing::info!("MCP server running on STDIO");

    // Wait for shutdown: either the service completes or we receive a signal
    tokio::select! {
        result = service.waiting() => {
            result.context("MCP server error")?;
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received shutdown signal");
        }
    }

    tracing::info!("MCP server shut down");
    Ok(())
}

/// Resolve the API token from CLI args or token file.
fn resolve_cli_token(cli: &Cli) -> Result<String> {
    match (&cli.token, &cli.token_file) {
        (Some(_), Some(_)) => {
            anyhow::bail!("specify either --token or --token-file, not both");
        }
        (Some(token), None) => Ok(token.clone()),
        (None, Some(path)) => {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read token file: {}", path.display()))?;
            let token = content.trim().to_string();
            if token.is_empty() {
                anyhow::bail!("token file is empty: {}", path.display());
            }
            Ok(token)
        }
        (None, None) => {
            anyhow::bail!(
                "API token required. Set TROVATO_API_TOKEN env var, \
                 use --token-file, or use --token."
            );
        }
    }
}

/// Initialize tracing with output to stderr.
///
/// MCP STDIO transport uses stdout for JSON-RPC messages, so all
/// logging must go to stderr to avoid corrupting the protocol.
fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .init();
}
