// This is a required file for rust libraries which declares what files are
// part of the library and what interfaces are public from the library.

// Declare that the c_api.rs file exists and is a public sub-namespace.
// C doesn't care about the namespaces, but Rust does.
pub mod c_api;

// Declare other .rs file/module exists, but make them private.
mod cache;
mod config;
mod logging;
mod network;
mod updater;
mod yaml;

// Take all public items from the updater namespace and make them public.
pub use self::updater::*;

#[cfg(not(test))]
// Exposes error!(), info!(), etc macros.
#[macro_use]
extern crate log;

#[cfg(test)]
extern crate tempdir;
