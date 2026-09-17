// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! `oxwin` — the one binary that ships.
//!
//! Clicked, it is the desktop app. Given arguments, it is the CLI. Both front
//! ends are libraries; all this crate decides is which one the invocation meant,
//! and that decision lives in [`dispatch::route`] as a pure function of the
//! argument list so it can be tested exhaustively.
//!
//! The `windows_subsystem` attribute belongs here rather than in `oxwin-gui`: a
//! subsystem is a property of a linked binary, and this is the binary. See
//! `console::attach` for what a GUI-subsystem executable has to do before it can
//! print, and `docs/superpowers/specs/2026-09-17-single-binary-design.md` for why
//! that trade-off is the one taken.

// Hide the console window on Windows release builds; clicking this is the point.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod console;
mod dispatch;

use anyhow::Context as _;
use dispatch::Route;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch::route(&args) {
        Route::Gui(opened_with) => {
            // eframe's error is not an `anyhow::Error`, and there is no terminal
            // to read it in anyway — this route is the one someone clicked.
            oxwin_gui::run(opened_with)
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("the app could not start")
        }
        Route::Cli => {
            console::attach();
            oxwin_cli::run(&args)
        }
    }
}
