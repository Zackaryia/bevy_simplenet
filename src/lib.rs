//features
#![feature(hash_drain_filter)]  //todo: nightly/unstable, will change to extract_if

//documentation
#![doc = include_str!("../README.md")]

//module tree
mod authentication;
mod common;
mod common_internal;
mod rate_limiter;
mod text_ping_pong;

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "server")]
mod server;

//API exports
pub use crate::authentication::*;
pub use crate::common::*;
pub(crate) use crate::common_internal::*;
pub use crate::rate_limiter::*;
pub(crate) use crate::text_ping_pong::*;

#[cfg(feature = "client")]
pub use crate::client::*;
#[cfg(feature = "server")]
pub use crate::server::*;
