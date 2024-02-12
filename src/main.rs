// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! wimsy: a playful way to manipulate Windows images for use in an Oxide rack.

use app::App;
use clap::Parser;

pub const UNATTEND_FILES: &[&str] = &[
    "Autounattend.xml",
    "cloudbase-init-unattend.conf",
    "cloudbase-init.conf",
    "OxidePrepBaseImage.ps1",
    "prep.cmd",
    "specialize-unattend.xml",
];

#[cfg(target_os = "illumos")]
mod illumos;
#[cfg(target_os = "illumos")]
use illumos::get_script;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::get_script;

#[cfg(not(any(target_os = "illumos", target_os = "linux")))]
compile_error!("only Linux and illumos targets are supported");

pub mod app;
pub mod autounattend;
pub mod runner;
pub mod steps;
pub mod ui;
pub mod util;

fn main() -> anyhow::Result<()> {
    let app = App::parse();
    let interactive = match app.interactive {
        Some(val) => val,
        None => atty::is(atty::Stream::Stdout),
    };

    let script = get_script(&app);
    runner::run_script(script, interactive, &app.work_dir)
}
