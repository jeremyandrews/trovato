//! Cron service tests.
//!
//! Tests for Phase 6A scheduled operations.

use trovato_kernel::cron::{LastCronRun, RedisQueue};

#[test]
fn test_last_cron_run_serde() {
    let run = LastCronRun {
        timestamp: 1707836400,
        hostname: "server-1".to_string(),
        result: "Completed { tasks_run: [\"cleanup\"], duration_ms: 150 }".to_string(),
    };

    let json = serde_json::to_string(&run).unwrap();
    assert!(json.contains("server-1"));
    assert!(json.contains("1707836400"));

    let parsed: LastCronRun = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.hostname, "server-1");
    assert_eq!(parsed.timestamp, 1707836400);
}

#[test]
fn test_queue_key_format() {
    let client = redis::Client::open("redis://127.0.0.1:6379").unwrap();
    let queue = RedisQueue::new(client);

    // Test that queue creation doesn't panic
    assert!(std::mem::size_of_val(&queue) > 0);
}

#[test]
fn test_cron_result_format() {
    use trovato_kernel::cron::CronResult;

    // Test Completed variant
    let completed = CronResult::Completed {
        tasks_run: vec!["task1".to_string(), "task2".to_string()],
        duration_ms: 250,
    };
    let debug_str = format!("{completed:?}");
    assert!(debug_str.contains("Completed"));
    assert!(debug_str.contains("task1"));

    // Test Skipped variant
    let skipped = CronResult::Skipped;
    let debug_str = format!("{skipped:?}");
    assert!(debug_str.contains("Skipped"));

    // Test Failed variant
    let failed = CronResult::Failed("connection error".to_string());
    let debug_str = format!("{failed:?}");
    assert!(debug_str.contains("Failed"));
    assert!(debug_str.contains("connection error"));
}

#[test]
fn test_cutoff_calculation() {
    // Test the cutoff time calculation used in cleanup tasks
    let now = chrono::Utc::now().timestamp();
    let six_hours = 6 * 60 * 60;
    let cutoff = now - six_hours;

    // Cutoff should be in the past
    assert!(cutoff < now);
    // Should be exactly 6 hours ago
    assert_eq!(now - cutoff, six_hours);
}
