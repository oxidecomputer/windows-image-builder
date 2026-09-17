// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The desktop app.
//!
//! The entry point is [`run`], not a `main`: the shipped binary is `oxwin`, which
//! decides between this and the CLI from its arguments. See `crates/oxwin`, which
//! also carries the `windows_subsystem` attribute that used to live here — the
//! subsystem is a property of the linked binary, so it belongs to whatever crate
//! produces one.

mod app;
mod icon;
mod stages;
mod stepper;
mod theme;

use std::path::PathBuf;

/// The app icon at any edge, as RGBA8. Exposed for `examples/icon-png.rs`, which
/// is how the macOS bundle's `.icns` is generated — see `tools/package-macos.sh`.
pub fn icon_rgba(edge: usize) -> Vec<u8> {
    icon::rgba(edge)
}

/// Open the app, optionally on a file.
///
/// `opened_with` is passed in rather than read from the environment: the
/// dispatcher has already decided that this argument is a path and not a
/// subcommand, and a second reader of `argv` is how the two front ends drift
/// apart.
pub fn run(opened_with: Option<PathBuf>) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([980.0, 720.0])
        .with_min_inner_size([760.0, 560.0])
        .with_title("Windows Image Builder")
        .with_icon(icon::icon())
        // The reverse-DNS id is what Wayland matches against a `.desktop` file and what
        // GNOME shows in place of the binary name; without it the window is labelled
        // `oxwin-gui`, which means nothing to the person running it.
        .with_app_id(APP_ID);

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
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, opened_with)))),
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

/// The app id the window carries. A `.desktop` file has to name the same string in
/// `StartupWMClass` or the shell cannot match the launcher to the window, and shows
/// a second generic entry in the dock while the app runs.
const APP_ID: &str = "com.oxide.windows-image-builder";

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair that has to agree is in two files, one of which is not Rust and so
    /// is never compiled. Renaming the app id without the `.desktop` file would
    /// break the Linux launcher silently.
    #[test]
    fn the_desktop_file_matches_the_app_id() {
        let desktop = include_str!("../../../tools/oxide-windows.desktop");
        assert!(
            desktop.contains(&format!("StartupWMClass={APP_ID}")),
            "tools/oxide-windows.desktop does not name the app id {APP_ID}"
        );
        // `Exec` has to carry `%f`, or opening an ISO with the launcher drops the
        // file and the app comes up empty.
        assert!(
            desktop.lines().any(|l| l.starts_with("Exec=") && l.contains("%f")),
            "the Exec line must pass the file through as %f"
        );
    }
}
