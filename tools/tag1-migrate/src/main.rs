//! Tag1.com → Trovato content migration tool.
//!
//! Reads Tag1's Eleventy source files and creates Trovato items via direct
//! database insertion. Run after the Trovato server has initialized the
//! database schema and plugin migrations.

mod blog;
mod categories;
mod pages;
mod redirects;
mod team;

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::PgPool;

#[derive(Parser)]
#[command(name = "tag1-migrate", about = "Migrate Tag1.com content to Trovato")]
struct Cli {
    /// Path to the Tag1 Eleventy source directory.
    #[arg(long, default_value = ".")]
    source: String,

    /// PostgreSQL connection string.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Only process this many items per type (0 = all).
    #[arg(long, default_value = "0")]
    limit: usize,

    /// Dry run: parse and validate but don't insert.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let source = std::path::Path::new(&cli.source);

    if !source.exists() {
        anyhow::bail!("Source directory does not exist: {}", source.display());
    }

    tracing::info!(source = %source.display(), "starting Tag1 migration");

    let pool = PgPool::connect(&cli.database_url)
        .await
        .context("failed to connect to database")?;

    // Phase 1: Create category vocabularies and terms
    tracing::info!("=== Phase 1: Categories ===");
    let tag_map = categories::migrate_categories(source, &pool, cli.dry_run)
        .await
        .context("category migration failed")?;
    tracing::info!(tags = tag_map.len(), "categories migrated");

    // Phase 2: Team members (needed before blog posts for author references)
    tracing::info!("=== Phase 2: Team Members ===");
    let team_map = team::migrate_team(source, &pool, cli.limit, cli.dry_run)
        .await
        .context("team migration failed")?;
    tracing::info!(members = team_map.len(), "team members migrated");

    // Phase 3: Blog posts, team talks, how-tos
    tracing::info!("=== Phase 3: Blog Posts & Articles ===");
    let blog_count = blog::migrate_blogs(source, &pool, &team_map, &tag_map, cli.limit, cli.dry_run)
        .await
        .context("blog migration failed")?;
    tracing::info!(count = blog_count, "articles migrated");

    // Phase 4: Composed pages (service pages, product pages, case studies, white papers)
    tracing::info!("=== Phase 4: Composed Pages ===");
    let page_count = pages::migrate_pages(source, &pool, cli.limit, cli.dry_run)
        .await
        .context("page migration failed")?;
    tracing::info!(count = page_count, "composed pages migrated");

    // Phase 5: Redirects
    tracing::info!("=== Phase 5: Redirects ===");
    let redirect_count = redirects::migrate_redirects(source, &pool, cli.dry_run)
        .await
        .context("redirect migration failed")?;
    tracing::info!(count = redirect_count, "redirects imported");

    tracing::info!("=== Migration complete ===");
    Ok(())
}
