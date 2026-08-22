//! wb-core: business logic pure library. No process/IO bindings —
//! daemon, cli, mcp, panel are all thin adapters over this.

#![recursion_limit = "256"]

pub mod ai;
pub mod commands;
pub mod error;
pub mod models;
pub mod paths;
pub mod protocol;
pub mod search;
pub mod storage;

pub use error::{CoreError, ErrorCode, Result};
