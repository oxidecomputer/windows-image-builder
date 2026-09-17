// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Which front end an invocation meant.
//!
//! Kept as a pure function of the argument list so every row of the table in
//! `docs/superpowers/specs/2026-09-17-single-binary-design.md` is a unit test. The
//! filesystem is the one thing it has to ask about, and it asks through a closure
//! for the same reason.

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// Open the app, optionally on a file.
    Gui(Option<PathBuf>),
    /// Run the CLI with these arguments. Includes the arguments that name no
    /// command at all — reporting that is the CLI's job, not this function's.
    Cli,
}

/// Arguments that ask for the CLI without naming a subcommand. `oxwin --help` has
/// to print usage rather than open a window, so these are recognised alongside
/// `oxwin_cli::COMMANDS` and excluded from that list.
const HELP: &[&str] = &["-h", "--help", "help"];

/// Ask for the app explicitly, whatever else is on the command line.
const GUI_FLAG: &str = "--gui";

/// Decide from the real filesystem. `args` has the program name removed.
pub fn route(args: &[String]) -> Route {
    route_with(args, |p| p.exists())
}

pub fn route_with(args: &[String], exists: impl Fn(&Path) -> bool) -> Route {
    let Some(first) = args.first().map(String::as_str) else {
        // Nothing at all: a double-click in Explorer, or a bundle launch.
        return Route::Gui(None);
    };

    if first == GUI_FLAG {
        return Route::Gui(path_arg(args.get(1), exists));
    }

    // A process serial number, which older macOS LaunchServices hands a bundled
    // app. Unrecognised, it would take the CLI's unknown-command path — so a click
    // would fail on exactly the machines the bundle exists for.
    if first.starts_with("-psn_") {
        return Route::Gui(None);
    }

    // The subcommand table is consulted before the filesystem, so a directory
    // named `build` in the working directory cannot shadow the `build` command.
    if oxwin_cli::COMMANDS.contains(&first) || HELP.contains(&first) {
        return Route::Cli;
    }

    // A path means the app, opened on that file: this is `oxwin some.iso`, and it
    // is also what macOS passes for a drop on the Dock icon or an "Open With".
    if let Some(path) = path_arg(args.first(), &exists) {
        return Route::Gui(Some(path));
    }

    // Anything else is a mistyped command. The CLI names it and exits 2; opening a
    // window instead would hide the typo.
    Route::Cli
}

fn path_arg(
    arg: Option<&String>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let arg = arg?;
    // A leading dash is a flag someone got wrong, not a file, even in the unlikely
    // event that a file by that name exists.
    if arg.starts_with('-') {
        return None;
    }
    let path = Path::new(arg);
    exists(path).then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Only `some.iso` and the directory `build` exist, so the tests can state what
    /// is on disk instead of creating it.
    fn fake_fs(p: &Path) -> bool {
        matches!(p.to_str(), Some("some.iso" | "build" | "/mnt/media"))
    }

    fn route_fake(list: &[&str]) -> Route {
        route_with(&args(list), fake_fs)
    }

    #[test]
    fn no_arguments_opens_the_app() {
        assert_eq!(route_fake(&[]), Route::Gui(None));
    }

    #[test]
    fn a_subcommand_runs_the_cli() {
        for cmd in oxwin_cli::COMMANDS {
            assert_eq!(
                route_fake(&[cmd]),
                Route::Cli,
                "{cmd} should be the CLI"
            );
        }
        assert_eq!(route_fake(&["build", "a.iso", "out.img"]), Route::Cli);
    }

    #[test]
    fn help_runs_the_cli() {
        for flag in HELP {
            assert_eq!(route_fake(&[flag]), Route::Cli, "{flag}");
        }
    }

    #[test]
    fn an_existing_path_opens_the_app_on_it() {
        assert_eq!(
            route_fake(&["some.iso"]),
            Route::Gui(Some(PathBuf::from("some.iso")))
        );
        // A mount point, which is the other thing the engine accepts.
        assert_eq!(
            route_fake(&["/mnt/media"]),
            Route::Gui(Some(PathBuf::from("/mnt/media")))
        );
    }

    /// The bug this orders against: a `build` directory in the working directory
    /// turning `oxwin build …` into a click.
    #[test]
    fn a_command_beats_a_file_of_the_same_name() {
        assert!(fake_fs(Path::new("build")), "the fixture must make this real");
        assert_eq!(route_fake(&["build"]), Route::Cli);
    }

    #[test]
    fn a_mistyped_command_is_a_cli_error_not_a_window() {
        assert_eq!(route_fake(&["buidl"]), Route::Cli);
        assert_eq!(route_fake(&["--verbose"]), Route::Cli);
        // A path that is not there is a mistake too, and the CLI says so.
        assert_eq!(route_fake(&["missing.iso"]), Route::Cli);
    }

    #[test]
    fn the_gui_can_be_asked_for_explicitly() {
        assert_eq!(route_fake(&["--gui"]), Route::Gui(None));
        assert_eq!(
            route_fake(&["--gui", "some.iso"]),
            Route::Gui(Some(PathBuf::from("some.iso")))
        );
        // A file that is not there is not worth refusing a window over; the app
        // opens on its first stage and asks for one.
        assert_eq!(route_fake(&["--gui", "missing.iso"]), Route::Gui(None));
    }

    #[test]
    fn a_process_serial_number_opens_the_app() {
        assert_eq!(route_fake(&["-psn_0_1234567"]), Route::Gui(None));
    }
}
