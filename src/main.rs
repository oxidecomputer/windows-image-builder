// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! wimsy: a playful way to manipulate Windows images for use in an Oxide rack.

use clap::Parser;

#[cfg(target_os = "illumos")]
mod illumos;
#[cfg(target_os = "illumos")]
use illumos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::*;

#[cfg(not(any(target_os = "illumos", target_os = "linux")))]
compile_error!("only Linux and illumos targets are supported");

pub mod runner;
pub mod steps;
pub mod util;

fn main() -> anyhow::Result<()> {
    let app = App::parse();
    runner::run_script(app.get_script())
}
