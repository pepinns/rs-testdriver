#![allow(unused_imports, unused_variables, dead_code)]
#![feature(proc_macro_hygiene, type_alias_impl_trait)]
#![feature(decl_macro)]
#![feature(exit_status_error)]
#![feature(associated_type_defaults)]

mod cmd;
pub mod driver;
pub mod error;
mod freeip;
#[cfg(test)]
pub mod mock_node;
pub mod nsdriver;
pub mod pool;

pub use async_process::{Command, Stdio};
pub use cmd::CmdBuilder;
pub use freeip::FreeIp;
pub use pool::*;
