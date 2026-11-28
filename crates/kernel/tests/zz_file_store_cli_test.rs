#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! BG16: the file store through the real binary.
//!
//! `trovato serve` refuses to start on a bucket it cannot use, before it
//! touches the database, so a broken store fails the deploy and not the first
//! upload. `trovato files migrate-to-s3` refuses without an S3 destination,
//! copies a directory and says so in one line, and exits nonzero naming a file
//! it could not copy.
//!
//! The S3 cases need the test store (`TROVATO_TEST_S3_ENDPOINT`,
//! `TROVATO_TEST_S3_BUCKET`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`); they
//! skip locally without it and fail under CI. No database is used: every
//! command here is given an address where nothing listens.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use uuid::Uuid;

/// Nothing listens here: proof that the commands under test never reach a
/// database.
const NO_DATABASE: &str = "postgres://trovato:trovato@127.0.0.1:1/none";

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("trovato-{label}-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The test store, or `None` (skip) outside CI.
struct Store {
    endpoint: String,
    bucket: String,
    key_id: String,
    secret: String,
}

fn store() -> Option<Store> {
    trovato_test_utils::env::load_dotenv();
    let get = |name: &str| std::env::var(name).ok();
    match (
        get("TROVATO_TEST_S3_ENDPOINT"),
        get("TROVATO_TEST_S3_BUCKET"),
        get("AWS_ACCESS_KEY_ID"),
        get("AWS_SECRET_ACCESS_KEY"),
    ) {
        (Some(endpoint), Some(bucket), Some(key_id), Some(secret)) => Some(Store {
            endpoint,
            bucket,
            key_id,
            secret,
        }),
        _ => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must run the file store CLI tests: the workflow starts MinIO for them"
            );
            eprintln!("skipping: no S3 test store (TROVATO_TEST_S3_ENDPOINT) configured");
            None
        }
    }
}

/// `trovato <args>` in a clean environment plus `env`, with a time limit, so
/// a check that was skipped fails the test instead of leaving a server up.
async fn run(work: &Path, env: &[(&str, &str)], args: &[&str]) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trovato"));
    cmd.env_clear()
        .current_dir(work)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("DATABASE_URL", NO_DATABASE)
        .env("REDIS_URL", "redis://127.0.0.1:1")
        .env("RUST_LOG", "warn")
        .env("UPLOADS_DIR", work.join("uploads"))
        .env("PLUGINS_DIR", work.join("plugins"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(Duration::from_secs(90), cmd.output())
        .await
        .expect("the command must finish on its own")
        .expect("run trovato");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), all)
}

fn s3_env<'a>(s: &'a Store, bucket: &'a str, prefix: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("FILE_STORAGE", "s3"),
        ("S3_BUCKET", bucket),
        ("S3_REGION", "us-east-1"),
        ("S3_ENDPOINT", s.endpoint.as_str()),
        ("S3_ALLOW_HTTP", "true"),
        ("S3_PREFIX", prefix),
        ("AWS_ACCESS_KEY_ID", s.key_id.as_str()),
        ("AWS_SECRET_ACCESS_KEY", s.secret.as_str()),
    ]
}

#[tokio::test]
async fn serve_refuses_a_bucket_it_cannot_use_before_the_database() {
    let Some(s) = store() else { return };
    let work = TempDir::new("serve-s3");
    let bucket = format!("no-such-bucket-{}", Uuid::now_v7().simple());
    let (code, out) = run(&work.0, &s3_env(&s, &bucket, "p"), &["serve"]).await;
    assert_ne!(code, Some(0), "{out}");
    assert!(out.contains("is not usable"), "{out}");
    assert!(out.contains(&bucket), "{out}");
    assert!(!out.contains(&s.secret), "the secret must never be printed");
}

#[tokio::test]
async fn migrate_to_s3_refuses_without_an_s3_destination() {
    let work = TempDir::new("migrate-nodest");
    let (code, out) = run(&work.0, &[], &["files", "migrate-to-s3"]).await;
    assert_ne!(code, Some(0), "{out}");
    assert!(out.contains("FILE_STORAGE=s3"), "{out}");
}

#[tokio::test]
async fn migrate_to_s3_copies_a_directory_and_reports_a_file_it_could_not() {
    let Some(s) = store() else { return };
    let work = TempDir::new("migrate-cli");
    let uploads = work.0.join("uploads");
    std::fs::create_dir_all(uploads.join("2026/11")).unwrap();
    std::fs::write(uploads.join("2026/11/a_note.txt"), b"one").unwrap();
    std::fs::write(uploads.join("2026/11/b_note.txt"), b"two").unwrap();
    let prefix = format!("cli-{}", Uuid::now_v7().simple());
    let bucket = s.bucket.clone();
    let env = s3_env(&s, &bucket, &prefix);

    let (code, out) = run(&work.0, &env, &["files", "migrate-to-s3"]).await;
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(&format!(
            "2 files under {}: 2 copied, 0 already in s3://{bucket}/{prefix}, 0 failed",
            uploads.display()
        )),
        "{out}"
    );

    std::fs::write(uploads.join("2026/bad\\name.txt"), b"x").unwrap();
    let (code, out) = run(
        &work.0,
        &env,
        &["files", "migrate-to-s3", "--parallel", "1"],
    )
    .await;
    assert_ne!(code, Some(0), "{out}");
    assert!(out.contains("failed: 2026/bad\\name.txt"), "{out}");
    assert!(out.contains("0 copied, 2 already in"), "{out}");
    assert!(out.contains("1 failed"), "{out}");
}

/// A store that cannot be reached at all is an outage, not a setting: `serve`
/// says so and goes on starting (here it then stops at the database, which
/// nothing serves), so a pod restarted during an S3 outage does not crash loop.
#[tokio::test]
async fn serve_starts_anyway_when_the_store_cannot_be_reached() {
    let work = TempDir::new("serve-unreachable");
    // Nothing listens on port 1.
    let env = [
        ("FILE_STORAGE", "s3"),
        ("S3_BUCKET", "unreachable"),
        ("S3_REGION", "us-east-1"),
        ("S3_ENDPOINT", "http://127.0.0.1:1"),
        ("S3_ALLOW_HTTP", "true"),
        ("AWS_ACCESS_KEY_ID", "unused"),
        ("AWS_SECRET_ACCESS_KEY", "unused-secret"),
    ];
    let (code, out) = run(&work.0, &env, &["serve"]).await;
    assert_ne!(code, Some(0), "the database is not there: {out}");
    assert!(out.contains("starting anyway"), "{out}");
    assert!(
        !out.contains("unused-secret"),
        "the secret must never be printed"
    );
}
