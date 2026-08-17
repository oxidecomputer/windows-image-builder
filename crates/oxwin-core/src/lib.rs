// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

pub mod assets;
pub mod bootstrap;
pub mod builder;
pub mod engine;
pub mod exfat;
pub mod fat32;
pub mod mbr;
pub mod progress;
pub mod settings;
pub mod sparse;
pub mod udf;
pub mod unattend;
pub mod wim;

pub use assets::Assets;
pub use engine::{Cancel, Engine};
pub use progress::{Event, Reporter};
pub use settings::{
    Credentials, Deployment, Experience, Problem, Settings, WindowsRelease,
    generate_password,
};
pub use udf::{Entry, UdfImage};
