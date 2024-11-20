#![allow(unused_imports, unused_variables, dead_code)]
#![feature(proc_macro_hygiene, type_alias_impl_trait)]
#![feature(decl_macro)]
#![feature(exit_status_error)]

mod cmd;
pub mod driver;
pub mod error;
mod freeip;

pub use async_process::{Command, Stdio};
pub use cmd::CmdBuilder;
pub use freeip::FreeIp;
