// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Structs, traits, and functions for defining and running a set of scripted
//! operations.

use std::{
    borrow::Cow,
    collections::HashMap,
    io::{Read, Write},
    process::Stdio,
};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

const PROGRESS_TICK_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

type StepFn = dyn Fn(&mut Context, &Ui) -> anyhow::Result<()>;

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
        func: impl Fn(&mut Context, &Ui) -> anyhow::Result<()> + 'static,
    ) -> Self {
        Self { label, func: Box::new(func), prereq_commands: Vec::new() }
    }

    pub fn with_prereqs(
        label: &'static str,
        func: impl Fn(&mut Context, &Ui) -> anyhow::Result<()> + 'static,
        commands: &[&'static str],
    ) -> Self {
        Self { label, func: Box::new(func), prereq_commands: commands.to_vec() }
    }

    pub fn prereq_commands(&self) -> &[&'static str] {
        self.prereq_commands.as_slice()
    }
}

/// Implemented by objects that can be used as scripts.
pub trait Script {
    /// Yields a slice of steps that can be executed to run this script.
    fn steps(&self) -> &[ScriptStep];

    fn print_configuration(
        &self,
        w: Box<dyn std::io::Write>,
    ) -> std::io::Result<()>;

    fn check_prerequisites(&self) -> Result<(), Vec<String>>;

    /// Yields a `HashMap` that contains key-value pairs that should be inserted
    /// into the script's `[Context]` prior to running it.
    fn initial_context(&self) -> HashMap<String, String>;
}

struct StepAndProgress<'a> {
    step: &'a ScriptStep,
    bar: ProgressBar,
}

/// Runs a script, pretty-printing its various labels and the outcomes of each
/// step.
pub fn run_script(
    script: Box<dyn Script>,
    interactive: bool,
) -> anyhow::Result<()> {
    script.print_configuration(Box::new(std::io::stdout()))?;
    println!("");

    if let Err(e) = script.check_prerequisites() {
        let s = "Some prerequisites were not satisfied:".bold();
        println!("{}", s);

        for unsatisfied in e.iter() {
            println!("  {}", unsatisfied);
        }

        println!("");
        anyhow::bail!("some script prerequisites weren't satisfied");
    }

    if interactive {
        println!("Press Enter to continue or CTRL-C to cancel.");
        std::io::stdout().flush()?;
        std::io::stdin().read(&mut [0u8])?;
    }

    let mut ctx = Context { vars: script.initial_context().clone() };
    let multi = interactive.then_some(MultiProgress::new());

    let steps_with_progress: Vec<StepAndProgress> = script
        .steps()
        .iter()
        .map(|step| {
            let bar = if let Some(multi) = &multi {
                multi.add(ProgressBar::new_spinner())
            } else {
                ProgressBar::new_spinner()
            };

            bar.set_message(step.label);
            bar.set_style(
                ProgressStyle::with_template("  {msg:.dim}").unwrap(),
            );
            bar.tick();
            StepAndProgress { step, bar }
        })
        .collect();

    for step in steps_with_progress {
        step.bar.set_style(ProgressStyle::default_spinner());
        step.bar.enable_steady_tick(PROGRESS_TICK_INTERVAL);
        let ui = Ui { current_step: &step, interactive };
        match (step.step.func)(&mut ctx, &ui) {
            Ok(()) => {
                step.bar.set_message(step.step.label);
                step.bar.set_style(
                    ProgressStyle::with_template("✓ {msg:.green}").unwrap(),
                );
                step.bar.finish();
            }
            Err(e) => {
                step.bar.set_style(
                    ProgressStyle::with_template("⚠ {msg:.bold.red}").unwrap(),
                );
                step.bar.finish();
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

pub struct Ui<'step, 'progress> {
    current_step: &'progress StepAndProgress<'step>,
    interactive: bool,
}

impl Ui<'_, '_> {
    pub fn set_substep(&self, substep: impl Into<Cow<'static, str>>) {
        let bar = &self.current_step.bar;
        bar.set_message(format!(
            "{}: {}",
            self.current_step.step.label,
            &substep.into()
        ));
    }

    pub fn stdout_target(&self) -> Stdio {
        if self.interactive {
            Stdio::piped()
        } else {
            Stdio::inherit()
        }
    }
}
