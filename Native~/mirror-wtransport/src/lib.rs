//! Native half of the Mirror WebTransport transport.
//!
//! Builds a `cdylib` that Unity loads through P/Invoke. It provides:
//!
//! * a WebTransport **server** built on [`wtransport`], which is what browser
//!   clients connect to, and
//! * a WebTransport **client**, so the transport also works in the editor and in
//!   standalone players instead of only in WebGL builds.
//!
//! The API surface is in [`ffi`]; [`common`] documents the wire protocol that
//! the browser client in `MirrorWTransport.jslib` implements as well.

mod client;
mod common;
mod ffi;
mod server;
mod session;

pub use ffi::*;
