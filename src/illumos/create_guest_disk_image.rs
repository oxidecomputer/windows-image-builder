// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{os::unix::net::UnixStream, process::Command, str::FromStr};

use crate::{
    runner::{Context, MissingPrerequisites, Script, ScriptStep, Ui},
    util::{
        check_executable_prerequisites, check_file_prerequisites,
        run_command_check_status,
    },
};

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use colored::Colorize;

const VNIC_NAME: &str = "vnic0";

pub struct CreateGuestDiskImageArgs {
    pub work_dir: Utf8PathBuf,
    pub output_image: Utf8PathBuf,
    pub vnic_link: String,
    pub installer_image: Utf8PathBuf,
    pub propolis_bootrom: Utf8PathBuf,
}

pub struct CreateGuestDiskImageScript {
    steps: Vec<ScriptStep>,
    args: CreateGuestDiskImageArgs,
}

impl CreateGuestDiskImageScript {
    pub(super) fn new(script_args: CreateGuestDiskImageArgs) -> Self {
        Self { steps: get_script(), args: script_args }
    }
}

impl Script for CreateGuestDiskImageScript {
    fn steps(&self) -> &[ScriptStep] {
        self.steps.as_slice()
    }

    fn print_configuration(
        &self,
        mut w: Box<dyn std::io::Write>,
    ) -> std::io::Result<()> {
        writeln!(
            w,
            "Installing Windows in propolis-standalone with these options:\n"
        )?;

        let args = &self.args;
        writeln!(w, "  {}: {}", "Working directory".bold(), args.work_dir)?;
        writeln!(w, "  {}: {}", "Installer disk".bold(), args.installer_image)?;
        writeln!(w, "  {}: {}", "Guest bootrom".bold(), args.propolis_bootrom)?;
        writeln!(w, "  {}: {}", "VNIC physical link".bold(), args.vnic_link)?;
        writeln!(w, "  {}: {}", "VNIC name".bold(), VNIC_NAME)?;
        writeln!(w, "")?;
        writeln!(w, "  {}: {}", "Output file".bold(), args.output_image)?;

        Ok(())
    }

    fn check_prerequisites(&self) -> MissingPrerequisites {
        let mut errors = Vec::new();
        let files = vec![
            self.args.installer_image.clone(),
            self.args.propolis_bootrom.clone(),
        ];

        errors.extend(check_file_prerequisites(&files).into_iter());
        errors.extend(check_executable_prerequisites(self.steps()).into_iter());

        MissingPrerequisites::from_messages(errors, vec![])
    }

    fn initial_context(&self) -> std::collections::HashMap<String, String> {
        let args = &self.args;
        [
            ("work_dir".to_string(), args.work_dir.to_string()),
            ("vnic_link".to_string(), args.vnic_link.clone()),
            ("vnic_name".to_string(), VNIC_NAME.to_string()),
            ("installer_image".to_string(), args.installer_image.to_string()),
            ("output_image".to_string(), args.output_image.to_string()),
            ("propolis_bootrom".to_string(), args.propolis_bootrom.to_string()),
        ]
        .into_iter()
        .collect()
    }
}

fn create_vnic(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    run_command_check_status(
        Command::new("pfexec").args([
            "dladm",
            "create-vnic",
            "-t",
            "-l",
            ctx.get_var("vnic_link").unwrap(),
            ctx.get_var("vnic_name").unwrap(),
        ]),
        ui,
    )
    .map(|_| ())
}

fn create_output_image(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    crate::steps::create_output_image(ctx.get_var("output_image").unwrap(), ui)
}

fn write_vm_toml(ctx: &mut Context, _ui: &dyn Ui) -> Result<()> {
    let mut vm_toml_path =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();
    vm_toml_path.push("vm.toml");

    std::fs::write(
        &vm_toml_path,
        format!(
            r#"
[main]
name = "wimsy-server"
cpus = 2
memory = 2048
bootrom = "{}"

[block_dev.win_image]
type = "file"
path = "{}"
[dev.block0]
driver = "pci-nvme"
block_dev = "win_image"
pci-path = "0.16.0"

[block_dev.win_iso]
type = "file"
path = "{}"
[dev.block1]
driver = "pci-nvme"
block_dev = "win_iso"
pci-path = "0.17.0"

[dev.net0]
driver = "pci-virtio-viona"
vnic = "{}"
pci-path = "0.8.0"
"#,
            ctx.get_var("propolis_bootrom").unwrap(),
            ctx.get_var("output_image").unwrap(),
            ctx.get_var("installer_image").unwrap(),
            ctx.get_var("vnic_name").unwrap()
        ),
    )
    .context("writing temporary vm.toml to disk")
    .map(|_| ())?;

    ctx.set_var("vm_toml_path", vm_toml_path.to_string());
    Ok(())
}

fn run_propolis_standalone(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    let current_dir = std::env::current_dir().context(
        "getting current directory before launching propolis-standalone",
    )?;

    let work_dir =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();

    std::env::set_current_dir(&work_dir).context(
        "setting working directory before launching propolis-standalone",
    )?;

    let mut propolis = Command::new("pfexec");
    propolis
        .args(["propolis-standalone", ctx.get_var("vm_toml_path").unwrap()])
        .stdout(ui.stdout_target())
        .stderr(ui.stdout_target());

    ui.set_substep(&format!("Launching propolis-standalone: {:?}", propolis));
    let mut propolis =
        propolis.spawn().context("spawning propolis-standalone")?;

    let mut ttya_path = work_dir.clone();
    ttya_path.push("ttya");

    ui.set_substep("Waiting for propolis-standalone to create ttya");
    for _ in 0..=5 {
        if ttya_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Grant read privileges on ttya to everyone so this process can connect
    // to it. Note that doing this immediately after the socket is created can
    // race with further setup operations from propolis-standalone that remove
    // these permissions. This was not needed in the script-based version of
    // this procedure because `nc(1)` does not pass any X/Open versioning flags
    // to sockfs `connect`. `UnixStream::connect` does, and under this standard
    // the connector needs write access to be able to connect to the socket.
    ui.set_substep("Waiting for propolis-standalone to finish setting up ttya");
    std::thread::sleep(std::time::Duration::from_secs(5));
    run_command_check_status(
        Command::new("pfexec").args(["chmod", "666", ttya_path.as_str()]),
        ui,
    )?;

    let _stream = UnixStream::connect(&ttya_path)
        .context("connecting to propolis-standalone's ttya")?;

    ui.set_substep(
        "Waiting for propolis-standalone to exit (this may take a while)",
    );

    let status =
        propolis.wait().context("waiting for propolis-standalone to exit")?;

    if !status.success() {
        anyhow::bail!("propolis-server exited with error {:?}", status);
    }

    std::env::set_current_dir(current_dir).context(
        "restoring working directory after running propolis-standalone",
    )?;

    Ok(())
}

fn get_partition_size(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    let (sector_size, last_sector) =
        crate::steps::get_output_image_partition_size(
            ctx.get_var("output_image").unwrap(),
            ui,
        )?;

    ctx.set_var("sector_size", sector_size);
    ctx.set_var("last_sector", last_sector);
    Ok(())
}

fn shrink_output_image(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    crate::steps::shrink_output_image(
        ctx.get_var("output_image").unwrap(),
        ctx.get_var("sector_size").unwrap(),
        ctx.get_var("last_sector").unwrap(),
        ui,
    )
}

fn repair_secondary_gpt(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    crate::steps::repair_secondary_gpt(ctx.get_var("output_image").unwrap(), ui)
}

fn remove_vnic(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    run_command_check_status(
        Command::new("pfexec").args([
            "dladm",
            "delete-vnic",
            ctx.get_var("vnic_name").unwrap(),
        ]),
        ui,
    )
    .map(|_| ())
}

fn get_script() -> Vec<ScriptStep> {
    vec![
        ScriptStep::new("create VNIC for installation VM", create_vnic),
        ScriptStep::with_prereqs(
            "create output image",
            create_output_image,
            &["qemu-img"],
        ),
        ScriptStep::new("write config TOML for installation VM", write_vm_toml),
        ScriptStep::with_prereqs(
            "run installation in propolis-standalone",
            run_propolis_standalone,
            &["propolis-standalone"],
        ),
        ScriptStep::with_prereqs(
            "get size of primary installation partition",
            get_partition_size,
            &["sgdisk"],
        ),
        ScriptStep::with_prereqs(
            "trim unused sectors from output image",
            shrink_output_image,
            &["qemu-img"],
        ),
        ScriptStep::with_prereqs(
            "repair secondary GPT in output image",
            repair_secondary_gpt,
            &["sgdisk"],
        ),
        ScriptStep::new("remove installation VM VNIC", remove_vnic),
    ]
}
