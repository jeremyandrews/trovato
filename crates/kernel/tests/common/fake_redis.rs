//! A fake Redis that accepts connections and then stops answering (B247).
//!
//! Included by path (`#[path = "common/fake_redis.rs"]`) from the tests that
//! need it, so they do not pull in the whole shared harness.
//!
//! The fake answers the client's connection setup (`CLIENT SETINFO` and the
//! like) with `+OK`, so the failure lands on a real command, and then does
//! what its [`Mode`] says.

#![allow(dead_code)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    /// Answer nothing, not even the setup: the connection never completes.
    Silent,
    /// Answer the setup, then hold the socket open and never answer the
    /// command, as a peer behind a black-holed route does.
    SilentOnCommand,
    /// Answer the setup, then reset the connection mid-command.
    ResetOnCommand,
}

/// Start the fake Redis and return its URL.
pub async fn fake_redis(mode: Mode) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // Every accepted socket is kept until the runtime shuts down, so a
        // silent peer stays silent rather than closing.
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            match mode {
                Mode::Silent => held.push(socket),
                _ => {
                    tokio::spawn(serve(socket, mode));
                }
            }
        }
    });
    format!("redis://{addr}/")
}

/// Answer connection setup (`CLIENT SETINFO` and friends) with `+OK`, then
/// stop answering or reset on the first real command.
async fn serve(mut socket: TcpStream, mode: Mode) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        while let Some((name, used)) = parse_command(&buf) {
            buf.drain(..used);
            if matches!(name.as_str(), "CLIENT" | "HELLO" | "SELECT" | "AUTH") {
                if socket.write_all(b"+OK\r\n").await.is_err() {
                    return;
                }
                continue;
            }
            match mode {
                Mode::ResetOnCommand => {
                    // Linger zero turns the close into a reset.
                    let _ = socket.set_zero_linger();
                    drop(socket);
                    return;
                }
                _ => {
                    // Hold the socket, never answer.
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    return;
                }
            }
        }
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Parse one RESP array command from the front of `buf`: its upper-cased
/// name and the bytes it took, or `None` if it is not all there yet.
fn parse_command(buf: &[u8]) -> Option<(String, usize)> {
    fn line(buf: &[u8], at: usize) -> Option<(&[u8], usize)> {
        let rest = buf.get(at..)?;
        let end = rest.windows(2).position(|w| w == b"\r\n")?;
        Some((&rest[..end], at + end + 2))
    }
    let (head, mut at) = line(buf, 0)?;
    let count: usize = std::str::from_utf8(head.strip_prefix(b"*")?)
        .ok()?
        .parse()
        .ok()?;
    let mut name = String::new();
    for i in 0..count {
        let (len, next) = line(buf, at)?;
        let len: usize = std::str::from_utf8(len.strip_prefix(b"$")?)
            .ok()?
            .parse()
            .ok()?;
        let data = buf.get(next..next + len)?;
        if i == 0 {
            name = String::from_utf8_lossy(data).to_ascii_uppercase();
        }
        at = next + len + 2;
        if buf.len() < at {
            return None;
        }
    }
    Some((name, at))
}

/// User plus system CPU this process has used so far.
pub fn cpu_time() -> Duration {
    // SAFETY: `getrusage` only writes the struct it is given.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    assert_eq!(rc, 0, "getrusage failed");
    let tv = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
    };
    tv(usage.ru_utime) + tv(usage.ru_stime)
}
