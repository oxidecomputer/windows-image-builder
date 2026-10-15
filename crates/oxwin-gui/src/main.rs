// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The app on its own, for development.
//!
//! What ships is `oxwin` — one binary that decides between this and the CLI from
//! its arguments (see `crates/oxwin`). This target exists so `cargo run -p
//! oxwin-gui -- some.iso` keeps working.

// Hide the console window on Windows release builds; this is a GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    let opened_with = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists());
    oxwin_gui::run(opened_with)
}
