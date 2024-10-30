// This is a required file for rust libraries which declares what files are
// part of the library and what interfaces are public from the library.

#[macro_use]
mod logging_macros;

// Declare that the c_api.rs file exists and is a public sub-namespace.
// C doesn't care about the namespaces, but Rust does.
pub mod c_api;

// Declare other .rs file/module exists, but make them private.
mod cache;
mod config;
mod events;
mod logging;
mod network;
mod time;
mod updater;
mod updater_lock;
mod yaml;

#[cfg(any(target_os = "android", test))]
mod android;

#[cfg(test)]
mod test_utils;

// Take all public items from the updater namespace and make them public.
pub use self::updater::*;

#[cfg(test)]
extern crate tempdir;
