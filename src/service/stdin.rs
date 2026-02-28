use std::io::BufRead;

use tokio::sync::mpsc;

/// Global stdin reader service.
///
/// Runs a single persistent blocking thread that reads lines from lazymc's stdin
/// and sends them through the channel. The server process consumes these lines
/// when it is running.
///
/// This must be a single global task (not per-server-invocation) to avoid
/// zombie `spawn_blocking` threads competing for stdin reads after server restarts.
pub async fn service(sender: mpsc::UnboundedSender<String>) {
    tokio::task::spawn_blocking(move || {
        let stdin = std::io::stdin();
        loop {
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    warn!(target: "lazymc", "Failed to read from stdin: {}", err);
                    break;
                }
            }
        }
    })
    .await
    .ok();
}
