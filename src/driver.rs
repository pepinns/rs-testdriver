use async_trait::async_trait;
use futures::{
    io::BufReader, AsyncBufReadExt, AsyncRead, AsyncReadExt, FutureExt, StreamExt, TryFutureExt,
    TryStreamExt,
};
use std::{future::Future, io::BufRead, time::Duration};
use tokio::time::timeout;
use tracing::{debug, error, info, trace};

use crate::drain::drain;
use crate::error::Error;
use async_process::{Child, ChildStderr, ChildStdout, Command};

// type EmptyResultFuture = impl Future<Output = Result<(), Error>>;

/// Where a [`Driver`] is with the child's stdout.
enum Stdout {
    /// Not piped: there is nothing to read, so nothing for a strategy to wait on.
    NotPiped,
    /// Piped, and read only by the strategy, while it waits in [`Driver::wait_for_ready`].
    Waiting(BufReader<ChildStdout>),
    /// The strategy is satisfied, and a background task reads whatever the child prints from here on.
    Draining,
}

/// Runs a child process and waits for it to be ready.
///
/// The child's stderr is read, and logged, from the moment the driver is created. Its stdout is
/// read by the strategy in [`wait_for_ready`](Driver::wait_for_ready) and, once that returns `Ok`,
/// by a background task: so **call `wait_for_ready`**, or stdout is never read, and a child that
/// prints enough to fill the pipe blocks.
///
/// Everything the child prints is logged at `info`, one event per line, under the target
/// `testdriver::child` (`child stdout: ...` / `child stderr: ...`). A test's captured logs show it
/// when the test fails; `testdriver::child=off` silences it. Bytes that are not UTF-8 are logged
/// lossily.
pub struct Driver<T> {
    pub child: Child,
    pub strategy: T,
    stdout: Stdout,
}

/// Read `reader` until the child closes it, logging each line.
fn spawn_drain<R>(reader: R, stream: &'static str)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let result = drain(reader, |line| {
            info!(target: "testdriver::child", "child {stream}: {line}");
        })
        .await;
        if let Err(e) = result {
            debug!("stopped reading the child's {stream}: {e}");
        }
    });
}

impl<T> Driver<T>
where
    T: Strategy,
{
    pub fn new(mut child: Child, strategy: T) -> Self {
        if let Some(stderr) = child.stderr.take() {
            spawn_drain(stderr, "stderr");
        }
        let stdout = match child.stdout.take() {
            Some(stdout) => Stdout::Waiting(BufReader::new(stdout)),
            None => Stdout::NotPiped,
        };
        Self {
            child,
            strategy,
            stdout,
        }
    }

    /// Wait, for up to `duration`, until the strategy says the child is ready.
    ///
    /// On success the rest of the child's stdout is read in the background from then on. After a
    /// failure or a timeout it is not, and the call can be repeated.
    pub async fn wait_for_ready(&mut self, duration: Duration) -> Result<(), Error> {
        let out = match &mut self.stdout {
            // Ready already: it is the drain task that has stdout now.
            Stdout::Draining => return Ok(()),
            Stdout::NotPiped => {
                error!("the child's stdout is not piped, so there is nothing to wait for");
                return Err(Error::Unknown);
            }
            Stdout::Waiting(out) => out,
        };

        let wait_future = self.strategy.wait_for_ready(out);
        timeout(duration, wait_future).await??;

        if let Stdout::Waiting(out) = std::mem::replace(&mut self.stdout, Stdout::Draining) {
            spawn_drain(out, "stdout");
        }
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<(), Error> {
        self.child.kill()?;
        let wait_future = self.child.status().map(|res| {
            trace!("Results: {:?}", &res);
            let o: Result<(), Error> = match res {
                Ok(status) => Ok(()),
                Err(e) => Err(e.into()),
            };
            o
        });
        timeout(Duration::from_secs(10), wait_future).await?
    }
}

impl<T> Drop for Driver<T> {
    fn drop(&mut self) {
        self.child.kill().unwrap_or(());
    }
}

#[async_trait]
pub trait Strategy {
    async fn wait_for_ready(&mut self, out: &mut BufReader<ChildStdout>) -> Result<(), Error>;
}

pub struct StdoutStrategy {
    pub match_str: String,
    pub ready: bool,
}
impl StdoutStrategy {
    pub fn new(match_str: impl Into<String>) -> Self {
        Self {
            match_str: match_str.into(),
            ready: false,
        }
    }
}
#[async_trait]
impl Strategy for StdoutStrategy {
    async fn wait_for_ready(&mut self, out: &mut BufReader<ChildStdout>) -> Result<(), Error> {
        if self.ready {
            return Ok(());
        }

        let mut linestream = out.lines();

        loop {
            match linestream.next().await {
                Some(Ok(line)) => {
                    // handle line by matching.

                    if line.contains(&self.match_str) {
                        info!("{} matched {}", line, &self.match_str);
                        self.ready = true;
                        return Ok(());
                    } else {
                        debug!("{} not matched {}", &line, &self.match_str);
                    }
                }
                Some(Err(e)) => {
                    return Err(e.into());
                }
                None => break,
            }
        }

        //return err if not matched
        error!("Did not match anything");
        Err(Error::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use async_process::{Command, Stdio};
    use futures::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn it_finds_output_line() {
        let cmd = Command::new("echo")
            .stdout(Stdio::piped())
            .arg("No\nNo\nNot Match\nYay\nYay!\n")
            .spawn()
            .expect("Failed to start command");

        let mut driver = Driver::new(
            cmd,
            StdoutStrategy {
                match_str: "Yay!".to_string(),
                ready: false,
            },
        );

        driver
            .wait_for_ready(Duration::from_secs(2))
            .await
            .expect("Expect it to complete");
    }

    #[tokio::test]
    async fn it_times_out() {
        let cmd = Command::new("cat")
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to start command");

        let mut driver = Driver::new(
            cmd,
            StdoutStrategy {
                match_str: "Yay!".to_string(),
                ready: false,
            },
        );

        driver
            .wait_for_ready(Duration::from_secs(2))
            .await
            .expect_err("Expect it to complete");
    }

    #[tokio::test]
    async fn it_can_be_stopped() {
        let mut cmd = Command::new("cat")
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .spawn()
            .expect("Failed to start command");

        let mut stdin = cmd.stdin.take().expect("Should get stdin");

        let mut driver = Driver::new(
            cmd,
            StdoutStrategy {
                match_str: "Yay!".to_string(),
                ready: false,
            },
        );

        let res = stdin.write_all("Some\nOutput\nYay!\n".as_bytes()).await;

        driver
            .wait_for_ready(Duration::from_secs(2))
            .await
            .expect("Expect it to complete");

        driver.stop().await.expect("Expected driver to stop");
    }

    // ~1.2 MB of output: far more than a pipe buffer holds (64 KiB by default, less under some
    // container runtimes), so a child writing it blocks unless somebody reads the other end.
    const FLOOD: &str = "i=0; while [ $i -lt 20000 ]; do \
        echo \"line $i aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"; i=$((i+1)); done";

    /// Run `script` under `sh -c` with stdout and stderr piped, wait until it prints `ready` on
    /// stdout, then require that it runs to completion. A child that blocks on a full pipe never
    /// does, so this fails if the driver stops reading either stream.
    async fn child_finishes_after_ready(script: String) {
        let cmd = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to start command");
        let mut driver = Driver::new(cmd, StdoutStrategy::new("ready"));

        driver
            .wait_for_ready(Duration::from_secs(5))
            .await
            .expect("Expect it to become ready");

        let status = timeout(Duration::from_secs(10), driver.child.status())
            .await
            .expect("child is blocked writing to a pipe nobody reads")
            .expect("Expect an exit status");
        assert!(status.success(), "child failed: {status:?}");
    }

    #[tokio::test]
    async fn it_keeps_draining_stdout_after_ready() {
        child_finishes_after_ready(format!("echo ready; {FLOOD}; echo finished")).await;
    }

    #[tokio::test]
    async fn it_keeps_draining_stderr() {
        child_finishes_after_ready(format!("echo ready; {FLOOD} >&2; echo finished")).await;
    }

    #[tokio::test]
    async fn it_keeps_draining_stdout_past_invalid_utf8() {
        child_finishes_after_ready(format!(
            "echo ready; printf '\\377\\376 not utf-8\\n'; {FLOOD}; echo finished"
        ))
        .await;
    }

    #[tokio::test]
    async fn it_keeps_draining_stderr_past_invalid_utf8() {
        child_finishes_after_ready(format!(
            "printf '\\377\\376 not utf-8\\n' >&2; {FLOOD} >&2; echo ready; echo finished"
        ))
        .await;
    }

    #[tokio::test]
    async fn it_keeps_draining_a_line_with_no_newline() {
        // Far more bytes than a sane line, never terminated.
        child_finishes_after_ready(format!(
            "echo ready; {FLOOD} | tr -d '\\n'; echo finished"
        ))
        .await;
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    #[tokio::test]
    async fn it_keeps_draining_stderr_past_invalid_utf8_with_trace_logging() {
        // `trace!` only evaluates its arguments when trace is enabled, so a bad byte on stderr
        // used to panic the drain task only for whoever had turned trace logging on.
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Captured::default())
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _guard = tracing_subscriber::util::SubscriberInitExt::set_default(subscriber);

        child_finishes_after_ready(format!(
            "printf '\\377\\376 not utf-8\\n' >&2; {FLOOD} >&2; echo ready; echo finished"
        ))
        .await;
    }

    #[tokio::test]
    async fn it_logs_what_the_child_prints_after_ready() {
        // Tests show captured logs when they fail, so this is how a failing test shows what the
        // process it drove was saying.
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        let _guard = tracing_subscriber::util::SubscriberInitExt::set_default(subscriber);

        let cmd = Command::new("sh")
            .arg("-c")
            .arg("echo ready; echo out-after-ready; echo err-after-ready >&2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to start command");
        let mut driver = Driver::new(cmd, StdoutStrategy::new("ready"));
        driver
            .wait_for_ready(Duration::from_secs(5))
            .await
            .expect("Expect it to become ready");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let logged = loop {
            let logged = String::from_utf8_lossy(&captured.0.lock().unwrap()).to_string();
            if (logged.contains("child stdout: out-after-ready")
                && logged.contains("child stderr: err-after-ready"))
                || std::time::Instant::now() > deadline
            {
                break logged;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            logged.contains("child stdout: out-after-ready"),
            "stdout after ready was not logged:\n{logged}"
        );
        assert!(
            logged.contains("child stderr: err-after-ready"),
            "stderr was not logged:\n{logged}"
        );
    }
}
