// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Structs, traits, and functions for defining and running a set of scripted
//! operations.

use std::{
    collections::HashMap,
    io::{Read, Write},
};

use camino::Utf8Path;
use colored::Colorize;

use crate::ui::{Mode, Ui};

type StepFn = dyn Fn(&mut Context, &dyn crate::ui::Ui) -> anyhow::Result<()>;

/// A step in a scripted procedure.
pub struct ScriptStep {
    /// A descriptive label for this procedure step.
    label: &'static str,

    /// The function to execute to run this procedure step.
    func: Box<StepFn>,

    /// A list of commands that this step expects to launch via
    /// [`std::process::Command`]. The script runner uses these to check for
    /// missing dependencies before running the script.
    prereq_commands: Vec<&'static str>,
}

impl ScriptStep {
    pub fn new(
        label: &'static str,
        func: impl Fn(&mut Context, &dyn Ui) -> anyhow::Result<()> + 'static,
    ) -> Self {
        Self { label, func: Box::new(func), prereq_commands: Vec::new() }
    }

    pub fn with_prereqs(
        label: &'static str,
        func: impl Fn(&mut Context, &dyn Ui) -> anyhow::Result<()> + 'static,
        commands: &[&'static str],
    ) -> Self {
        Self { label, func: Box::new(func), prereq_commands: commands.to_vec() }
    }

    pub fn prereq_commands(&self) -> &[&'static str] {
        self.prereq_commands.as_slice()
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn run(&self, ctx: &mut Context, ui: &dyn Ui) -> anyhow::Result<()> {
        (self.func)(ctx, ui)
    }
}

/// Describes a set of files or commands a script expects to be present but that
/// appear to be missing.
#[derive(Default)]
pub struct MissingPrerequisites {
    /// A set of strings describing fatal errors--i.e., conditions that will
    /// prevent the script from working at all.
    errors: Vec<String>,

    /// A set of strings describing mere warnings--i.e., conditions that might
    /// prevent the script from working as intended, but that the user might
    /// also have intended and therefore know are safe to ignore.
    warnings: Vec<String>,
}

impl MissingPrerequisites {
    pub fn from_messages(errors: Vec<String>, warnings: Vec<String>) -> Self {
        Self { errors, warnings }
    }

    pub fn add_error(&mut self, error: String) {
        self.errors.push(error)
    }

    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning)
    }
}

/// Implemented by objects that can be used as scripts.
pub trait Script {
    /// Yields a slice of steps that can be executed to run this script.
    fn steps(&self) -> &[ScriptStep];

    /// Prints a message to the specified writer describing what this script
    /// will do.
    fn print_configuration(
        &self,
        w: Box<dyn std::io::Write>,
    ) -> std::io::Result<()>;

    /// Checks that this script's prerequisites are
    fn check_prerequisites(&self) -> MissingPrerequisites;

    /// Yields a `HashMap` that contains key-value pairs that should be inserted
    /// into the script's [`Context`] prior to running it.
    fn initial_context(&self) -> HashMap<String, String>;
}

/// Runs a script, pretty-printing its various labels and the outcomes of each
/// step.
pub fn run_script(
    script: Box<dyn Script>,
    interactive: bool,
    work_dir: &Utf8Path,
) -> anyhow::Result<()> {
    script.print_configuration(Box::new(std::io::stdout()))?;
    println!();

    let missing = script.check_prerequisites();
    if !missing.errors.is_empty() {
        println!("{}", "Some prerequisites were not satisfied:".bold());
        for error in missing.errors.iter() {
            println!("  {}", error);
        }

        println!();
        if !missing.warnings.is_empty() {
            println!("The following warnings were also raised:");
            for warning in missing.warnings.iter() {
                println!("  {}", warning);
            }

            println!();
        }

        anyhow::bail!("some script prerequisites weren't satisfied");
    } else if !missing.warnings.is_empty() {
        println!("{}", "Warning! Some prerequisites may be missing:".bold());
        for warning in missing.warnings.iter() {
            println!("  {}", warning);
        }

        println!();
    }

    println!("  Command logs will be written to {}\n", work_dir);

    if interactive {
        println!("Press Enter to continue or CTRL-C to cancel.");
        std::io::stdout().flush()?;
        std::io::stdin().read_exact(&mut [0u8])?;
    }

    let ctx = Context { vars: script.initial_context().clone() };
    let mode =
        if interactive { Mode::Interactive } else { Mode::NonInteractive };
    crate::ui::run_script(script, ctx, work_dir, mode)
}

/// A shared script execution context, provided to each step in a running
/// script. Each context contains a key-value store that individual steps can
/// use to pass values to future steps. The [`Script`] trait's `initial_context`
/// function allows each script to populate the store before the script
/// executes.
pub struct Context {
    vars: HashMap<String, String>,
}

impl Context {
    /// Gets the value of the supplied `var`, returning `None` if the value is
    /// not in the store.
    pub fn get_var(&self, var: &str) -> Option<&str> {
        self.vars.get(var).map(|v| v.as_str())
    }

    /// Sets the value of the supplied `var` to `value`, returning the old value
    /// if one was present.
    pub fn set_var(&mut self, var: &str, value: String) -> Option<String> {
        self.vars.insert(var.to_owned(), value)
    }
}
