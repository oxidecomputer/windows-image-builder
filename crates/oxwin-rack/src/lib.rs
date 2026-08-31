// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Talking to an Oxide rack.
//!
//! Separate from `oxwin-core` on purpose. The core turns an ISO into bytes,
//! deterministically, out of four dependencies, and its golden suite runs in a fifth of
//! a second; putting tokio and a TLS stack in it would cost both of those properties for
//! code that has nothing to do with generating an image.
//!
//! # The async boundary
//!
//! The SDK is async. This crate's public API is not: it owns a current-thread runtime
//! and blocks on it internally, so callers stay on the thread-plus-channel model the GUI
//! already uses for builds. The runtime never escapes, and no caller needs `tokio` in
//! its own dependency tree.
//!
//! # Credentials
//!
//! Read from the file `oxide auth login` writes, never obtained by us. No token leaves
//! [`profile`].

pub mod golden;
pub mod instance;
pub mod profile;
pub mod upload;

pub use golden::{Keep, Names, Resource};
pub use instance::{Created, InstanceSpec, Leftovers};
pub use profile::{
    Profile, Selector, environment_profile, profiles, profiles_in,
};
pub use upload::{DiskSpec, INSTALLER_BLOCK_SIZE, Rack, Uploaded};
