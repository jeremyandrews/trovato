//! Reads one line from stdin as a password, prompting on stderr: how `user
//! create` and `user reset-password` take one, so it never appears in argv,
//! the shell history or the output.
//!
//! From a terminal the password is read with echo off, so what is typed is
//! not shown, and the prompt comes only then: a person typing gets one, piped
//! input does not, so it does not run into the command's own output.
//!
//! Turning echo off and back on both happen through `tcsetattr`, so a signal
//! landing in between used to leave the terminal echoing nothing: the process
//! died before its own restore ran. [`read_password_from_stdin`] is the one
//! place that turns echo off, so it is the one place that guards against
//! that: for as long as it is waiting on the read, SIGINT, SIGTERM and SIGHUP
//! are caught by a handler that restores the terminal itself, prints a
//! newline (so the shell's next prompt does not run into the line the signal
//! interrupted) and exits with the shell's usual 128+signal code. The handler
//! comes out again once the read returns, by normal return or error, so the
//! rest of the command keeps the signal behaviour it would otherwise have had.

use anyhow::{Context, Result};

/// Read one line from stdin as a password, prompting on stderr.
pub fn read_password_from_stdin(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    let line = if std::io::stdin().is_terminal() {
        read_line_unechoed(prompt)
    } else {
        read_line()
    };
    Ok(line
        .context("failed to read password from stdin")?
        .trim()
        .to_string())
}

fn read_line() -> std::io::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf)
}

/// Prompt on stderr and read one line from the terminal on stdin with echo
/// off, putting the terminal back as it was whatever the read did. The line
/// end is still echoed (`ECHONL`), so the output after it starts on a new line.
#[cfg(unix)]
fn read_line_unechoed(prompt: &str) -> std::io::Result<String> {
    use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

    let stdin = std::io::stdin();
    let saved = tcgetattr(&stdin)?;
    let mut quiet = saved.clone();
    quiet.local_modes.remove(LocalModes::ECHO);
    quiet.local_modes.insert(LocalModes::ECHONL);
    // Echo goes off before the prompt appears, so nothing typed in answer to
    // the prompt is ever shown.
    tcsetattr(&stdin, OptionalActions::Flush, &quiet)?;

    // Guards the window where echo is off: a SIGINT, SIGTERM or SIGHUP here
    // restores `saved` itself (see `signal_guard`) before the process exits,
    // and a panic unwinding past `guard`'s drop does the same.
    let guard = signal_guard::Guard::install(&stdin, saved.clone());
    eprint!("{prompt}");
    let line = read_line();
    let restored = guard.restore();
    let line = line?;
    restored?;
    Ok(line)
}

/// Without termios the prompt says the input will be shown, so nobody types
/// a password at it believing otherwise.
#[cfg(not(unix))]
fn read_line_unechoed(prompt: &str) -> std::io::Result<String> {
    eprint!("{prompt}(shown as typed; pipe it in to hide it) ");
    read_line()
}

/// Installs, for the span of one blocking read, a signal handler that undoes
/// exactly the `tcsetattr` the read is wrapped in.
///
/// Nothing here is specific to passwords: it only ever touches the termios
/// it was handed and the signal dispositions it saved. `rustix` exposes
/// `tcgetattr`/`tcsetattr` but its `sigaction` equivalent is behind a
/// `runtime` feature documented as being for implementing things like libc,
/// not for applications, and the standard library has no way at all to
/// install a signal handler. `libc`'s raw `sigaction` is the narrow addition
/// that does, and it is already in the dependency tree transitively (most
/// crates that talk to the OS pull it in); this is the first direct use of it.
#[cfg(unix)]
mod signal_guard {
    use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

    use parking_lot::Mutex;
    use rustix::termios::{OptionalActions, Termios, tcsetattr};

    /// The three signals a terminal-driving shell can send a foreground
    /// process: Ctrl-C (SIGINT), a plain `kill`/service stop (SIGTERM), and
    /// the controlling terminal going away (SIGHUP).
    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// What the signal handler restores, written once before the handler is
    /// installed and read only by the handler, never mutated concurrently
    /// with a read: the `Mutex` is for interior mutability under a shared
    /// static, not for excluding a concurrent writer. `parking_lot`'s, not
    /// the standard library's, so neither the handler nor `Guard` has to
    /// handle (or discard, with an `unwrap` this crate's lints forbid outside
    /// tests) a poison error.
    static RESTORE: Mutex<Option<(RawFd, Termios)>> = Mutex::new(None);

    /// Exit status a shell expects after a process dies to a signal: 128 +
    /// the signal number (130 for SIGINT), the same convention every other
    /// `trovato` exit path leaves it to follow for signals it does not catch.
    fn exit_code_for(signal: libc::c_int) -> i32 {
        128 + signal
    }

    extern "C" fn handle(signal: libc::c_int) {
        // `try_lock`, not `lock`: a signal handler must never block, and the
        // only other place that ever holds this lock (`Guard::install`,
        // `take_restore`) holds it only to move a value in or out, never
        // across a syscall.
        if let Some(restore) = RESTORE.try_lock()
            && let Some((fd, termios)) = restore.as_ref()
        {
            // SAFETY: `fd` is stdin's descriptor for the lifetime of the
            // `Guard` that put it here; the handler only runs while that
            // `Guard` is installed.
            let fd = unsafe { BorrowedFd::borrow_raw(*fd) };
            let _ = tcsetattr(fd, OptionalActions::Now, termios);
        }
        // A bare `write`, not `println!`: the allocation and buffering behind
        // the usual print macros are not signal-safe to run from here.
        unsafe {
            libc::write(libc::STDERR_FILENO, b"\n".as_ptr().cast(), 1);
        }
        // `_exit`, not `std::process::exit`: it skips atexit/Drop machinery
        // that is not safe to run from a signal handler.
        unsafe {
            libc::_exit(exit_code_for(signal));
        }
    }

    /// Takes the saved restore state out, if a `drop` hasn't already.
    fn take_restore() -> Option<(RawFd, Termios)> {
        RESTORE.lock().take()
    }

    /// Installed signal dispositions for [`SIGNALS`], restored verbatim so a
    /// command that catches these signals itself (none does today, but
    /// nothing here should assume that) gets its own handler back, not the
    /// default.
    pub struct Guard {
        saved: [libc::sigaction; SIGNALS.len()],
        done: bool,
    }

    impl Guard {
        pub fn install(stdin: &std::io::Stdin, termios: Termios) -> Guard {
            *RESTORE.lock() = Some((stdin.as_raw_fd(), termios));

            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = handle as *const () as usize;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            action.sa_flags = 0;

            let mut saved = [unsafe { std::mem::zeroed() }; SIGNALS.len()];
            for (slot, signal) in saved.iter_mut().zip(SIGNALS) {
                // SAFETY: `action` is a valid, fully-initialized `sigaction`;
                // `slot` is valid to write the previous one into.
                unsafe {
                    libc::sigaction(signal, &action, slot);
                }
            }
            Guard { saved, done: false }
        }

        /// Puts the signal dispositions and the terminal back, and answers
        /// with the terminal restore's own result. Idempotent: the drop that
        /// follows a normal call sees nothing left to restore.
        pub fn restore(mut self) -> std::io::Result<()> {
            self.undo_signals();
            match take_restore() {
                Some((fd, termios)) => {
                    // SAFETY: `fd` is stdin's descriptor, open for as long
                    // as this process is reading from it.
                    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
                    Ok(tcsetattr(fd, OptionalActions::Now, &termios)?)
                }
                None => Ok(()),
            }
        }

        fn undo_signals(&mut self) {
            if self.done {
                return;
            }
            for (signal, saved) in SIGNALS.iter().zip(self.saved.iter()) {
                // SAFETY: `saved` was filled in by `install`'s own call to
                // `sigaction` for this same signal.
                unsafe {
                    libc::sigaction(*signal, saved, std::ptr::null_mut());
                }
            }
            self.done = true;
        }
    }

    impl Drop for Guard {
        /// Catches the panic path: `restore` already did this on every other
        /// path, and leaves `done` set so this is a no-op there. Errors from
        /// the terminal restore have nowhere to go from a `drop`, so this is
        /// best-effort, same as any other `Drop`.
        fn drop(&mut self) {
            if self.done {
                return;
            }
            self.undo_signals();
            if let Some((fd, termios)) = take_restore() {
                // SAFETY: `fd` is stdin's descriptor, open for as long as
                // this process is reading from it.
                let fd = unsafe { BorrowedFd::borrow_raw(fd) };
                let _ = tcsetattr(fd, OptionalActions::Now, &termios);
            }
        }
    }
}
