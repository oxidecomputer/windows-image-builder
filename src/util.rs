// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Utility functions shared by multiple scripts, possibly across multiple
//! target OSes.

use std::process::{Command, Output};

use crate::runner::Ui;

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
