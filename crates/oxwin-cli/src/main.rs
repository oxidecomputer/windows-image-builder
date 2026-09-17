// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The CLI on its own, for development.
//!
//! What ships is `oxwin` — one binary that decides between this and the GUI from
//! its arguments (see `crates/oxwin`). This target exists so that working on the
//! CLI does not mean linking egui on every build, and so the commands in
//! `CLAUDE.md` keep working.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    oxwin_cli::run(&args)
}
