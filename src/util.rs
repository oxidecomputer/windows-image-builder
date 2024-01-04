// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Utility functions shared by multiple scripts, possibly across multiple
//! target OSes.

use std::{
    collections::BTreeSet,
    process::{Command, Output},
};

use camino::Utf8PathBuf;

use crate::runner::{ScriptStep, Ui};

// TODO(gjc) drop this in favor of a real UI affordance
pub fn print_step_message(_msg: &str) {
    // println!("  {}", msg);
}

/// Runs a `Command` and returns its output. Returns `Err` if the command's exit
/// status indicates that it failed.
pub fn run_command_check_status(
    cmd: &mut Command,
    ui: &Ui,
) -> anyhow::Result<Output> {
    ui.set_substep(format!("{} {:?}", "executing: ", cmd));
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "'{}' returned non-success exit code: {:?}",
            cmd.get_program().to_string_lossy(),
            output
        );
    }

    Ok(output)
}

/// Runs the supplied `cmd` and searches its `stdout` for the first line
/// containing `row_contains`, then splits it by whitespace and returns the
/// `column`th zero-indexed word from that line.
///
/// Note that all searches are case-sensitive.
pub fn grep_command_for_row_and_column(
    cmd: &mut Command,
    row_contains: &str,
    column: usize,
    ui: &Ui,
) -> anyhow::Result<String> {
    let output = run_command_check_status(cmd, ui)?.stdout;
    let output = String::from_utf8_lossy(&output);
    for line in output.lines() {
        if !line.contains(row_contains) {
            continue;
        }

        return Ok(line
            .split_whitespace()
            .nth(column)
            .ok_or(anyhow::anyhow!(
                "matching line '{line}' does not have column index {column}"
            ))?
            .to_owned());
    }

    anyhow::bail!(
        "'{row_contains}' not found in output of {} (output: {:?})",
        cmd.get_program().to_string_lossy(),
        output
    );
}

/// Checks each file in `files` to make sure that it exists and is a file.
/// Returns a `Vec` of strings describing any missing or incorrectly-typed
/// files, or an empty `Vec` if all the files are present.
pub fn check_file_prerequisites(files: &[Utf8PathBuf]) -> Vec<String> {
    let mut errors = Vec::new();
    for file in files {
        if !file.exists() {
            errors.push(format!("'{}' not found", file));
        } else if !file.is_file() {
            errors.push(format!("'{}' exists but isn't a file", file));
        }
    }

    errors
}

pub fn check_executable_prerequisites(steps: &[ScriptStep]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut executables = BTreeSet::new();
    for step in steps {
        for dep in step.prereq_commands() {
            executables.insert(dep);
        }
    }

    for dep in executables {
        if let Err(e) = which::which(dep) {
            errors.push(format!(
                "binary or command '{}' not found (is it on your PATH?): {}",
                dep, e
            ));
        }
    }

    errors
}
