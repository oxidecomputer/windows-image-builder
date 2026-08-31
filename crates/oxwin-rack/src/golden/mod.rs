// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The golden-image cycle: media in, a reusable Windows image out.
//!
//! Seven steps — build, upload, instance, watch, snapshot, image, teardown — each
//! idempotent, each reconciled against the rack before it runs. See
//! `docs/superpowers/specs/2026-08-31-golden-image-automation-design.md`.

pub mod keep;
pub mod names;
pub mod steps;
pub mod watch;

pub use keep::{Keep, Resource};
pub use names::Names;
pub use steps::{pick_address, port_open};
pub use watch::{Action, Milestone, Watch};
