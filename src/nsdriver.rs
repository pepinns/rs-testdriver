use std::io::prelude::*;
use std::io::BufReader;
use std::io::Lines;
use std::time::{Duration, Instant};

use anyhow::bail;
use anyhow::Result;
use unshare::{Child, PipeReader, Stdio};

pub struct NsDriver {
    child: Child,
    // stdout: Lines<BufReader<PipeReader>>,
    stdout: Option<PipeReader>,
    name: String,
}

impl NsDriver {
    pub fn start(mut cmd: unshare::Command, name: String) -> Result<Self> {
        match cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let err = child.stderr.take().unwrap();

                let name_copy = name.clone();
                std::thread::spawn(move || {
                    let bf = std::io::BufReader::new(err).lines();

                    for l in bf {
                        match l {
                            Ok(line) => println!("[{}](stderr): {}", &name_copy, line),
                            Err(e) => {
                                println!("Error from nsdriver {}", &e);
                                return;
                            }
                        }
                    }
                });
                Ok(Self {
                    // stdout: std::io::BufReader::new(child.stdout.take().unwrap()).lines(),
                    stdout: child.stdout.take(),
                    child,
                    name,
                })
            }
            Err(e) => bail!("error spawning process"),
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        self.child.signal(unshare::Signal::SIGTERM)?;
        std::thread::sleep(Duration::from_millis(200));
        self.child.kill()?;
        let st = self.child.wait()?;
        Ok(())
    }

    pub fn consume_stdout(&mut self) {
        let stdout = std::io::BufReader::new(self.stdout.take().unwrap()).lines();
        let name = self.name.clone();
        std::thread::spawn(move || {
            for l in stdout {
                match l {
                    Ok(line) => println!("[{}](stdout): {}", &name, line),
                    Err(e) => {
                        println!("Error from nsdriver stdout {}", &e);
                        return;
                    }
                }
            }
        });
    }

    pub fn wait_for_line(&mut self, pattern: String, timeout: Duration) -> Result<()> {
        let deadline = Instant::now().checked_add(timeout).unwrap();

        let mut stdout = std::io::BufReader::new(self.stdout.take().unwrap()).lines();
        while Instant::now().le(&deadline) {
            let l = stdout.next();
            match l {
                Some(Ok(line)) => {
                    println!("Got line {:?}", &line);
                    if line.contains(&pattern) {
                        let name = self.name.clone();
                        std::thread::spawn(move || {
                            for l in stdout {
                                match l {
                                    Ok(line) => println!("[{}](stdout): {}", &name, line),
                                    Err(e) => {
                                        println!("Error from nsdriver stdout {}", &e);
                                        return;
                                    }
                                }
                            }
                        });
                        return Ok(());
                    }
                }
                Some(Err(e)) => {
                    tracing::error!(err=?e, "Error in BufReader lines");
                    bail!("errro reading lines from process");
                }
                None => {
                    bail!("No more lines left in process");
                }
            }
        }

        bail!("Not ready");
    }
}
