use std::{fmt::Debug, process::ExitStatusError};

use tokio::time::error::Elapsed;

#[derive(Debug)]
pub enum Error {
    Unknown,
    IoError(std::io::Error),
    TokioTimeoutError(Elapsed),
    ExitStatusError(ExitStatusError),
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Driver Error: {:?}", self)
    }
}

impl From<std::io::Error> for Error {
    fn from(other: std::io::Error) -> Self {
        Self::IoError(other)
    }
}
impl From<Elapsed> for Error {
    fn from(e: Elapsed) -> Self {
        Self::TokioTimeoutError(e)
    }
}
impl From<ExitStatusError> for Error {
    fn from(e: ExitStatusError) -> Self {
        Self::ExitStatusError(e)
    }
}
