use async_trait::async_trait;
use futures::{
    io::BufReader, AsyncBufReadExt, AsyncReadExt, FutureExt, StreamExt, TryFutureExt, TryStreamExt,
};
use std::{future::Future, io::BufRead, time::Duration};
use tokio::time::timeout;
use tracing::{debug, error, info, trace};

use crate::error::Error;
use async_process::{Child, ChildStderr, ChildStdout, Command};

// type EmptyResultFuture = impl Future<Output = Result<(), Error>>;

pub struct Driver<T> {
    pub child: Child,
    pub strategy: T,
    stdout: BufReader<ChildStdout>,
    // stderr: BufReader<ChildStderr>,
}

impl<T> Driver<T>
where
    T: Strategy,
{
    pub fn new(mut child: Child, strategy: T) -> Self {
        if child.stderr.is_some() {
            let stderr = BufReader::new(child.stderr.take().unwrap());
            tokio::spawn(async move {
                stderr
                    .lines()
                    .for_each(|line| {
                        trace!("child stderr: {}", line.unwrap());
                        futures::future::ready(())
                    })
                    .await;
            });
        }
        if child.stdout.is_some() {
            let stdout = BufReader::new(child.stdout.take().unwrap());
            return Self {
                child,
                strategy,
                stdout,
            };
        }
        todo!("wtf")
    }
    pub async fn wait_for_ready(&mut self, duration: Duration) -> Result<(), Error> {
        let wait_future = self.strategy.wait_for_ready(&mut self.stdout);
        timeout(duration, wait_future).await?
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
}
