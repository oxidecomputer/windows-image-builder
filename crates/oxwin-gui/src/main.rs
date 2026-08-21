// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

// Hide the console window on Windows release builds; this is a GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod stages;
mod stepper;
mod theme;

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([980.0, 720.0])
        .with_min_inner_size([760.0, 560.0])
        .with_title("Windows Image Builder");

    // Pin the window to a known spot when asked, so a screenshot can target this
    // window's rectangle alone. Without it the only option is capturing the whole
    // display, which sweeps up whatever else the user happens to have open.
    if let Some(spec) = std::env::var_os("OXWIN_WINDOW_AT") {
        let spec = spec.to_string_lossy().into_owned();
        let parts: Vec<f32> =
            spec.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if let [x, y] = parts[..] {
            viewport = viewport.with_position([x, y]);
        } else {
            eprintln!(
                "OXWIN_WINDOW_AT should look like \"120,120\", got {spec:?}"
            );
        }
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(
        "Windows Image Builder",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    );

    // Quitting cleanly still trips a macOS crash reporter dialog: as the window tears
    // down, AppKit's Touch Bar observation calls removeObserver:forKeyPath: for an
    // observer that is no longer registered, that Objective-C exception is rethrown
    // through a C++ terminate handler, and the process takes SIGABRT. It happens
    // entirely inside AppKit after our event loop has returned, so there is nothing on
    // our side left to fix or flush.
    //
    // Exiting here skips that teardown. Safe because the event loop has already
    // finished and no build thread outlives it — a build in flight keeps the window
    // open. Revisit if winit fixes it upstream, or if this app ever gains state that
    // must be flushed on exit.
    if result.is_ok() {
        std::process::exit(0);
    }
    result
}
