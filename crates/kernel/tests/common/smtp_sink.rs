//! A throwaway SMTP server for tests that need to prove a mail was sent.
//!
//! Proving delivery needs something to deliver to. The alternative was a capture
//! mode on `EmailService`, which means test-only behaviour inside production code
//! on the path that sends mail to real people. This binds a loopback port,
//! speaks enough SMTP for lettre to finish a delivery, and remembers what it was
//! given, so the assertions are about bytes on a socket rather than about an
//! internal flag.
//!
//! Point a `TestApp` at it by setting `smtp_host` to `127.0.0.1`, `smtp_port` to
//! [`SmtpSink::port`] and `smtp_encryption` to `"none"`.

use std::sync::{Arc, Mutex};

/// One message as the server received it.
#[derive(Default, Debug, Clone)]
pub struct Envelope {
    /// Addresses from `RCPT TO`, which is where the mail was actually going.
    pub recipients: Vec<String>,
    /// Everything between `DATA` and the terminating dot.
    pub data: String,
}

/// A loopback SMTP server that accepts everything and remembers it.
pub struct SmtpSink {
    pub port: u16,
    messages: Arc<Mutex<Vec<Envelope>>>,
}

impl SmtpSink {
    /// Bind an ephemeral port and start accepting.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port for the SMTP sink");
        let port = listener
            .local_addr()
            .expect("read the sink's local address")
            .port();
        let messages: Arc<Mutex<Vec<Envelope>>> = Arc::new(Mutex::new(Vec::new()));

        let accepted = Arc::clone(&messages);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let messages = Arc::clone(&accepted);
                tokio::spawn(async move {
                    serve_one_session(stream, messages).await;
                });
            }
        });

        Self { port, messages }
    }

    /// Every message received so far.
    pub fn messages(&self) -> Vec<Envelope> {
        self.messages.lock().expect("sink mutex").clone()
    }
}

/// Speak enough SMTP for lettre to deliver one message.
///
/// The EHLO reply advertises no extensions on purpose: nothing to negotiate
/// means no STARTTLS upgrade attempt against a server that has no certificate.
async fn serve_one_session(stream: tokio::net::TcpStream, messages: Arc<Mutex<Vec<Envelope>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut envelope = Envelope::default();

    if write.write_all(b"220 sink.test ESMTP\r\n").await.is_err() {
        return;
    }

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let command = line.trim_end().to_string();
        let upper = command.to_ascii_uppercase();

        let reply: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            b"250 sink.test\r\n"
        } else if upper.starts_with("RCPT TO") {
            envelope.recipients.push(address_in(&command));
            b"250 OK\r\n"
        } else if upper.starts_with("DATA") {
            if write
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await
                .is_err()
            {
                return;
            }
            let mut data = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                if line.trim_end() == "." {
                    break;
                }
                data.push_str(&line);
            }
            envelope.data = data;
            // Recorded before the acceptance is written, so a caller whose send
            // has returned is guaranteed to see it.
            messages
                .lock()
                .expect("sink mutex")
                .push(std::mem::take(&mut envelope));
            b"250 Queued\r\n"
        } else if upper.starts_with("QUIT") {
            let _ = write.write_all(b"221 Bye\r\n").await;
            return;
        } else if upper.starts_with("RSET") {
            envelope = Envelope::default();
            b"250 OK\r\n"
        } else {
            // MAIL FROM, NOOP, and anything else this does not need to model.
            b"250 OK\r\n"
        };

        if write.write_all(reply).await.is_err() {
            return;
        }
    }
}

/// The address inside `RCPT TO:<someone@example.test>`.
fn address_in(command: &str) -> String {
    match (command.find('<'), command.rfind('>')) {
        (Some(open), Some(close)) if close > open + 1 => command[open + 1..close].to_string(),
        // No angle brackets is legal enough for a sink; keep whatever followed the colon.
        _ => command
            .split_once(':')
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_default(),
    }
}
