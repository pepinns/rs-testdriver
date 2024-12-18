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

// TODO: make this async
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
                let out = child.stdout.take().unwrap();

                let name_copy = name.clone();
                let name_out_copy = name.clone();
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
                std::thread::spawn(move || {
                    let bf = std::io::BufReader::new(out).lines();

                    for l in bf {
                        match l {
                            Ok(line) => println!("[{}](stdout): {}", &name_out_copy, line),
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
    pub fn kill(&mut self) -> Result<()> {
        Ok(self.child.kill()?)
    }
    pub fn stop(&mut self) -> Result<()> {
        self.child.signal(unshare::Signal::SIGTERM)?;
        std::thread::sleep(Duration::from_millis(1000));
        let _ = self.child.kill();
        let st = self.child.wait()?;
        Ok(())
    }

    // pub fn consume_stdout(&mut self) {
    //     // let stdout = std::io::BufReader::new(self.stdout.take().unwrap()).lines();
    //     // let name = self.name.clone();
    //     // std::thread::spawn(move || {
    //     //     for l in stdout {
    //     //         match l {
    //     //             Ok(line) => println!("[{}](stdout): {}", &name, line),
    //     //             Err(e) => {
    //     //                 println!("Error from nsdriver stdout {}", &e);
    //     //                 return;
    //     //             }
    //     //         }
    //     //     }
    //     // });
    // }

    // TODO replicate wait_for_line using Watch channels in the main stdout loop
    // IDEA: have a way to program 'events' that are looked for/fired to a specific watch channel
    // that can then be waited on by callers.
    // pub fn wait_for_line(&mut self, pattern: String, timeout: Duration) -> Result<()> {
    //     return Ok(());
    //     let deadline = Instant::now().checked_add(timeout).unwrap();
    //
    //     let mut stdout = std::io::BufReader::new(self.stdout.take().unwrap()).lines();
    //     println!("Got lines");
    //     while Instant::now().le(&deadline) {
    //         println!("Try to read stdout");
    //         let l = stdout.next();
    //         match l {
    //             Some(Ok(line)) => {
    //                 println!("Got line {:?}", &line);
    //                 if line.contains(&pattern) {
    //                     let name = self.name.clone();
    //                     std::thread::spawn(move || {
    //                         for l in stdout {
    //                             match l {
    //                                 Ok(line) => println!("[{}](stdout): {}", &name, line),
    //                                 Err(e) => {
    //                                     println!("Error from nsdriver stdout {}", &e);
    //                                     return;
    //                                 }
    //                             }
    //                         }
    //                     });
    //                     return Ok(());
    //                 }
    //             }
    //             Some(Err(e)) => {
    //                 tracing::error!(err=?e, "Error in BufReader lines");
    //                 bail!("errro reading lines from process");
    //             }
    //             None => {
    //                 bail!("No more lines left in process");
    //             }
    //         }
    //     }
    //
    //     bail!("Not ready");
    // }
}
