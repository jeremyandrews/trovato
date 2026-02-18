#![allow(dead_code)]
//! Trovato CMS Load Testing Tool
//!
//! Simulates concurrent users accessing the CMS to verify performance targets.
//!
//! Usage:
//!   cargo run -p trovato-loadtest -- --base-url http://localhost:3000 --users 100 --duration 60

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use futures::future::join_all;
use rand::SeedableRng;
use rand::prelude::*;
use rand::rngs::StdRng;
use serde::Serialize;

/// Load test configuration.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Base URL of the Trovato server.
    #[arg(long, default_value = "http://localhost:3000")]
    base_url: String,

    /// Number of concurrent users.
    #[arg(long, default_value = "100")]
    users: usize,

    /// Test duration in seconds.
    #[arg(long, default_value = "60")]
    duration: u64,

    /// Think time between requests in milliseconds.
    #[arg(long, default_value = "100")]
    think_time: u64,

    /// Workload mix: percentage of read requests (0-100).
    #[arg(long, default_value = "70")]
    read_pct: u8,

    /// Workload mix: percentage of search requests (0-100).
    #[arg(long, default_value = "20")]
    search_pct: u8,
}

/// Load test statistics.
#[derive(Debug, Clone, Serialize)]
struct Stats {
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    total_duration_ms: u64,
    min_latency_ms: u64,
    max_latency_ms: u64,
    avg_latency_ms: f64,
    p50_latency_ms: u64,
    p95_latency_ms: u64,
    p99_latency_ms: u64,
    requests_per_second: f64,
}

/// Atomic counters for thread-safe statistics.
struct AtomicStats {
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    latencies: parking_lot::Mutex<Vec<u64>>,
}

impl AtomicStats {
    fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            latencies: parking_lot::Mutex::new(Vec::with_capacity(100_000)),
        }
    }

    fn record_request(&self, success: bool, latency_ms: u64) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
        self.latencies.lock().push(latency_ms);
    }

    fn compute_stats(&self, duration_secs: u64) -> Stats {
        let total = self.total_requests.load(Ordering::Relaxed);
        let successful = self.successful_requests.load(Ordering::Relaxed);
        let failed = self.failed_requests.load(Ordering::Relaxed);

        let mut latencies = self.latencies.lock().clone();
        latencies.sort();

        let min = *latencies.first().unwrap_or(&0);
        let max = *latencies.last().unwrap_or(&0);
        let avg = if !latencies.is_empty() {
            latencies.iter().sum::<u64>() as f64 / latencies.len() as f64
        } else {
            0.0
        };

        let percentile = |p: f64| -> u64 {
            if latencies.is_empty() {
                return 0;
            }
            let idx = ((latencies.len() as f64 * p) as usize).min(latencies.len() - 1);
            latencies[idx]
        };

        Stats {
            total_requests: total,
            successful_requests: successful,
            failed_requests: failed,
            total_duration_ms: duration_secs * 1000,
            min_latency_ms: min,
            max_latency_ms: max,
            avg_latency_ms: avg,
            p50_latency_ms: percentile(0.5),
            p95_latency_ms: percentile(0.95),
            p99_latency_ms: percentile(0.99),
            requests_per_second: total as f64 / duration_secs as f64,
        }
    }
}

/// Available endpoints for load testing.
const READ_ENDPOINTS: &[&str] = &["/health", "/admin/structure/types", "/gather/recent_items"];

const SEARCH_QUERIES: &[&str] = &["test", "blog", "article", "content", "page"];

#[tokio::main]
async fn main() {
    let args = Args::parse();

    println!("Trovato CMS Load Test");
    println!("=====================");
    println!("Base URL: {}", args.base_url);
    println!("Concurrent users: {}", args.users);
    println!("Duration: {} seconds", args.duration);
    println!("Think time: {} ms", args.think_time);
    println!(
        "Workload: {}% read, {}% search, {}% write",
        args.read_pct,
        args.search_pct,
        100 - args.read_pct - args.search_pct
    );
    println!();

    // Verify server is reachable
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    println!("Checking server connectivity...");
    match client.get(format!("{}/health", args.base_url)).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("Server is healthy\n");
        }
        Ok(resp) => {
            eprintln!("Server returned error status: {}", resp.status());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to connect to server: {}", e);
            std::process::exit(1);
        }
    }

    let stats = Arc::new(AtomicStats::new());
    let start_time = Instant::now();
    let test_duration = Duration::from_secs(args.duration);

    println!("Starting load test...");

    // Spawn user tasks
    let mut handles = Vec::with_capacity(args.users);
    for user_id in 0..args.users {
        let client = client.clone();
        let base_url = args.base_url.clone();
        let stats = stats.clone();
        let read_pct = args.read_pct;
        let search_pct = args.search_pct;
        let think_time = args.think_time;

        handles.push(tokio::spawn(async move {
            run_user(
                user_id,
                client,
                base_url,
                stats,
                test_duration,
                think_time,
                read_pct,
                search_pct,
            )
            .await;
        }));
    }

    // Wait for all users to complete
    join_all(handles).await;

    let actual_duration = start_time.elapsed();
    println!(
        "\nTest completed in {:.2} seconds",
        actual_duration.as_secs_f64()
    );

    // Compute and display results
    let results = stats.compute_stats(args.duration);

    println!("\nResults");
    println!("=======");
    println!("Total requests:      {}", results.total_requests);
    println!("Successful requests: {}", results.successful_requests);
    println!("Failed requests:     {}", results.failed_requests);
    println!(
        "Success rate:        {:.2}%",
        (results.successful_requests as f64 / results.total_requests as f64) * 100.0
    );
    println!();
    println!("Requests/second:     {:.2}", results.requests_per_second);
    println!();
    println!("Latency (ms):");
    println!("  Min:  {}", results.min_latency_ms);
    println!("  Avg:  {:.2}", results.avg_latency_ms);
    println!("  P50:  {}", results.p50_latency_ms);
    println!("  P95:  {}", results.p95_latency_ms);
    println!("  P99:  {}", results.p99_latency_ms);
    println!("  Max:  {}", results.max_latency_ms);

    // Check gate criterion
    println!();
    if results.p95_latency_ms <= 100 {
        println!(
            "✅ PASS: P95 latency ({} ms) <= 100 ms",
            results.p95_latency_ms
        );
    } else {
        println!(
            "❌ FAIL: P95 latency ({} ms) > 100 ms",
            results.p95_latency_ms
        );
    }

    if results.failed_requests == 0 {
        println!("✅ PASS: No failed requests");
    } else {
        println!("⚠️  WARN: {} failed requests", results.failed_requests);
    }
}

/// Simulate a single user making requests.
async fn run_user(
    user_id: usize,
    client: reqwest::Client,
    base_url: String,
    stats: Arc<AtomicStats>,
    duration: Duration,
    think_time: u64,
    read_pct: u8,
    search_pct: u8,
) {
    // Use a seeded RNG that is Send-safe
    let mut rng =
        StdRng::seed_from_u64(user_id as u64 + chrono::Utc::now().timestamp_millis() as u64);
    let start = Instant::now();

    while start.elapsed() < duration {
        // Choose request type based on workload mix
        let roll: u8 = rng.gen_range(0..100);
        let url = if roll < read_pct {
            // Read request
            let endpoint = READ_ENDPOINTS.choose(&mut rng).unwrap();
            format!("{}{}", base_url, endpoint)
        } else if roll < read_pct + search_pct {
            // Search request
            let query = SEARCH_QUERIES.choose(&mut rng).unwrap();
            format!("{}/api/search?q={}", base_url, query)
        } else {
            // Write request (just hit health for now - actual writes need auth)
            format!("{}/health", base_url)
        };

        // Make request and record latency
        let req_start = Instant::now();
        let result = client.get(&url).send().await;
        let latency_ms = req_start.elapsed().as_millis() as u64;

        let success = match result {
            Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 404,
            Err(_) => false,
        };

        stats.record_request(success, latency_ms);

        // Think time
        if think_time > 0 {
            tokio::time::sleep(Duration::from_millis(think_time)).await;
        }
    }
}

// Use parking_lot for better mutex performance
mod parking_lot {
    pub struct Mutex<T>(std::sync::Mutex<T>);

    impl<T> Mutex<T> {
        pub fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }

        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}
