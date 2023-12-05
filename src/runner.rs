// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Structs, traits, and functions for defining and running a set of scripted
//! operations.

use std::collections::HashMap;

use anyhow::Context as _;
use camino::Utf8PathBuf;
use colored::Colorize;

type StepFn = dyn Fn(&mut Context) -> anyhow::Result<()>;

/// A step in a scripted procedure.
pub struct ScriptStep {
    /// A descriptive label for this procedure step.
    label: &'static str,

    /// The function to execute to run this procedure step.
    func: Box<StepFn>,

    /// A list of commands that this step expects to launch via
    /// `[std::process::Command]`. The script runner uses these to check for
    /// missing dependencies before running the script.
    prereq_commands: Vec<&'static str>,
}

impl ScriptStep {
    pub fn new(
        label: &'static str,
        func: impl Fn(&mut Context) -> anyhow::Result<()> + 'static,
    ) -> Self {
        Self { label, func: Box::new(func), prereq_commands: Vec::new() }
    }

    pub fn with_prereqs(
        label: &'static str,
        func: impl Fn(&mut Context) -> anyhow::Result<()> + 'static,
        commands: &[&'static str],
    ) -> Self {
        Self { label, func: Box::new(func), prereq_commands: commands.to_vec() }
    }
}

/// Implemented by objects that can be used as scripts.
pub trait Script {
    /// Yields a slice of steps that can be executed to run this script.
    fn steps(&self) -> &[ScriptStep];

    /// Yields a `Vec` of paths to files that must exist for this script to run
    /// to completion.
    fn file_prerequisites(&self) -> Vec<Utf8PathBuf>;

    /// Yields a `HashMap` that contains key-value pairs that should be inserted
    /// into the script's `[Context]` prior to running it.
    fn initial_context(&self) -> HashMap<String, String>;
}

/// Checks that all of a script's prerequisites are satisfied.
fn check_script_prereqs(script: &dyn Script) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    for step in script.steps() {
        for command in &step.prereq_commands {
            if let Err(e) = which::which(command).with_context(|| {
                format!(
                    "checking prerequisite '{}' for script step '{}'",
                    command, step.label
                )
            }) {
                errors.push(e);
            }
        }
    }

    for file in script.file_prerequisites() {
        if !camino::Utf8Path::exists(&file) {
            errors.push(anyhow::anyhow!(
                "prerequisite file '{}' not found",
                file
            ));
        }
    }

    if !errors.is_empty() {
        anyhow::bail!("one or more prerequisites not satisfied: {:#?}", errors);
    }

    Ok(())
}

/// Runs a script, pretty-printing its various labels and the outcomes of each
/// step.
pub fn run_script(script: Box<dyn Script>) -> anyhow::Result<()> {
    check_script_prereqs(script.as_ref())?;
    let mut ctx = Context { vars: script.initial_context().clone() };
    for step in script.steps() {
        println!("[ .. ] {}", step.label.blue());
        match (step.func)(&mut ctx) {
            Ok(()) => println!("{} {}", "[ OK ]".green(), step.label.blue()),
            Err(e) => {
                println!("{} {}\n  {}", "[ !! ]".red(), step.label.blue(), e);
                return Err(e);
            }
        }
    }

    Ok(())
}

/// A shared script execution context, provided to each step in a running
/// script. Each context contains a key-value store that individual steps can
/// use to pass values to future steps. The `[Script]` trait's `initial_context`
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
