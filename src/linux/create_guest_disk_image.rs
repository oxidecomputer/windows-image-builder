// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Defines a script for building a Windows guest image on a Linux system using
//! QEMU.

use std::{collections::HashMap, io::Write, process::Command, str::FromStr};

use crate::{
    app::ImageSources,
    runner::{Context, MissingPrerequisites, Script, ScriptStep},
    ui::Ui,
    util::{
        check_executable_prerequisites, check_file_prerequisites,
        run_command_check_status,
    },
    UNATTEND_FILES,
};

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use colored::Colorize;

pub struct CreateGuestDiskImageArgs {
    pub work_dir: Utf8PathBuf,
    pub output_image: Utf8PathBuf,
    pub sources: ImageSources,
    pub ovmf_path: Utf8PathBuf,
    pub vga_console: bool,
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
            "Creating an Oxide-compatible Windows image with these options:\n"
        )?;

        let args = &self.args;
        let sources = &args.sources;
        writeln!(w, "  {}: {}", "Working directory".bold(), args.work_dir)?;
        writeln!(w, "  {}: {}", "Windows ISO".bold(), sources.windows_iso)?;
        writeln!(
            w,
            "  {}: {}",
            "Virtio driver ISO".bold(),
            sources.virtio_iso
        )?;
        writeln!(
            w,
            "  {}: {}",
            "Unattend file directory".bold(),
            sources.unattend_dir
        )?;
        writeln!(w, "  {}: {}", "Guest bootrom".bold(), args.ovmf_path)?;

        writeln!(w)?;

        if let Some(index) = sources.unattend_image_index {
            writeln!(
                w,
                "  Image index to insert into Autounattend.xml: {}",
                index
            )?;
        } else {
            writeln!(w, "  Will use default image index in Autounattend.xml")?;
        }

        if let Some(version) = sources.windows_version {
            writeln!(w, "  Target Windows version: {}", version)?;
        } else {
            writeln!(
                w,
                "  Will use default Windows version in Autounattend.xml"
            )?;
        }

        writeln!(w)?;
        writeln!(w, "  {}: {}", "Output file".bold(), args.output_image)?;

        Ok(())
    }

    fn check_prerequisites(&self) -> MissingPrerequisites {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut files = vec![
            self.args.sources.windows_iso.clone(),
            self.args.sources.virtio_iso.clone(),
            self.args.ovmf_path.clone(),
        ];

        // The ISOs and bootrom are strictly required to proceed.
        errors.extend(check_file_prerequisites(&files));

        // The unattend files are generally desirable, but it's possible to run
        // without them. For example:
        //
        // - The user may have supplied an ISO that already contains an
        //   Autounattend.xml.
        // - The user may have changed the installation scripts not to install
        //   cloudbase-init and so doesn't care about injecting its
        //   configuration files.
        files.clear();
        for file in UNATTEND_FILES {
            let mut path = self.args.sources.unattend_dir.clone();
            path.push(file);
            files.push(path);
        }

        warnings.extend(check_file_prerequisites(&files));

        // All the relevant executables are required to proceed.
        errors.extend(check_executable_prerequisites(self.steps()));

        MissingPrerequisites::from_messages(errors, warnings)
    }

    fn initial_context(&self) -> HashMap<String, String> {
        let args = &self.args;
        let mut ctx: HashMap<String, String> = [
            ("work_dir".to_string(), args.work_dir.to_string()),
            ("windows_iso".to_string(), args.sources.windows_iso.to_string()),
            ("virtio_iso".to_string(), args.sources.virtio_iso.to_string()),
            ("unattend_dir".to_string(), args.sources.unattend_dir.to_string()),
            ("output_image".to_string(), args.output_image.to_string()),
            ("ovmf_path".to_string(), args.ovmf_path.to_string()),
        ]
        .into_iter()
        .collect();

        if args.vga_console {
            ctx.insert("vga_console".to_string(), String::new());
        }

        ctx
    }
}

fn create_output_image(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    crate::steps::create_output_image(ctx.get_var("output_image").unwrap(), ui)
}

fn create_config_iso(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    let mut unattend_iso =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();
    unattend_iso.push("unattend.iso");
    run_command_check_status(
        Command::new("genisoimage").args([
            "-J",
            "-R",
            "-o",
            unattend_iso.as_str(),
            ctx.get_var("unattend_dir").unwrap(),
        ]),
        ui,
    )?;

    ctx.set_var("unattend_iso", unattend_iso.to_string());
    Ok(())
}

fn copy_unattend_files_to_work_dir(
    ctx: &mut Context,
    ui: &dyn Ui,
) -> Result<()> {
    let mut work_unattend =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();
    work_unattend.push("unattend");
    std::fs::create_dir_all(&work_unattend)
        .context("creating temporary directory for unattend files")?;

    let unattend_dir =
        Utf8PathBuf::from_str(ctx.get_var("unattend_dir").unwrap()).unwrap();

    for filename in UNATTEND_FILES {
        ui.set_substep(&format!("copying {}", filename));
        let mut src = unattend_dir.clone();
        src.push(filename);
        let mut dst = work_unattend.clone();
        dst.push(filename);
        if let Err(e) = std::fs::copy(&src, &dst) {
            match e.kind() {
                std::io::ErrorKind::NotFound => {}
                _ => {
                    return Err(e)
                        .with_context(|| format!("copying {}", filename))
                }
            }
        }
    }

    // Make subsequent steps use unattend files from the working copy.
    ctx.set_var("unattend_dir", work_unattend.to_string());
    Ok(())
}

fn customize_autounattend_xml(ctx: &mut Context, _ui: &dyn Ui) -> Result<()> {
    let customizer = crate::autounattend::AutounattendUpdater::new(
        ctx.get_var("unattend_image_index")
            .map(|val| val.parse::<u32>().unwrap()),
        None,
    );

    let unattend_dir =
        Utf8PathBuf::from_str(ctx.get_var("unattend_dir").unwrap()).unwrap();
    let mut unattend_src = unattend_dir.clone();
    unattend_src.push("Autounattend.tmp");
    let mut unattend_dst = unattend_dir.clone();
    unattend_dst.push("Autounattend.xml");
    std::fs::copy(&unattend_dst, &unattend_src)
        .context("creating temporary Autounattend.xml")?;

    customizer
        .run(&unattend_src, &unattend_dst)
        .context("customizing Autounattend.xml")?;

    std::fs::remove_file(&unattend_src)
        .context("removing temporary Autounattend.xml")?;

    Ok(())
}

fn install_via_qemu(ctx: &mut Context, ui: &dyn Ui) -> Result<()> {
    // Launch a VM in QEMU with the installation target disk attached as an NVMe
    // drive and CD-ROM drives containing the Windows installation media, the
    // virtio driver disk, and the answer file ISO created previously. Windows
    // setup will detect the presence of the answer file ISO and use the
    // Autounattend.xml located there to drive installation.
    let pflash_arg = format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ctx.get_var("ovmf_path").unwrap()
    );

    let install_disk_arg = format!(
        "if=none,id=drivec,file={},format=raw",
        ctx.get_var("output_image").unwrap()
    );

    let windows_iso_arg = format!(
        "file={},if=none,id=win-disk,media=cdrom",
        ctx.get_var("windows_iso").unwrap()
    );

    let virtio_iso_arg = format!(
        "file={},if=none,id=virtio-disk,media=cdrom",
        ctx.get_var("virtio_iso").unwrap()
    );

    let unattend_iso_arg = format!(
        "file={},if=none,id=unattend-disk,media=cdrom",
        ctx.get_var("unattend_iso").unwrap()
    );

    let mut args = vec![
        "-nodefaults",
        "-enable-kvm",
        "-M",
        "pc",
        "-m",
        "2048",
        "-cpu",
        "host,kvm=off,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time",
        "-smp",
        "2,sockets=1,cores=2",
        "-rtc",
        "base=localtime",
        "-drive",
        &pflash_arg,
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-pci,netdev=net0",
        "-device",
        "nvme,drive=drivec,serial=01de01de,physical_block_size=512,\
                logical_block_size=512,discard_granularity=512,bootindex=1",
        "-drive",
        &install_disk_arg,
        "-device",
        "ide-cd,drive=win-disk,id=cd-disk0,unit=0,bus=ide.0,bootindex=2",
        "-drive",
        &windows_iso_arg,
        "-device",
        "ide-cd,drive=virtio-disk,id=cd-disk1,unit=0,bus=ide.1",
        "-drive",
        &virtio_iso_arg,
        "-device",
        "ide-cd,drive=unattend-disk,id=cd-disk2,unit=1,bus=ide.0",
        "-drive",
        &unattend_iso_arg,
        // prep.cmd, the wrapper script that executes OxidePrepBaseImage.ps1,
        // redirects the child script's output to COM1 and will fail if no
        // serial device appears to be present, so `-serial` is required here.
        // Directing QEMU to write to stdio allows this function to decide what
        // to do with this output.
        "-serial",
        "stdio",
        // Set up the QEMU monitor to allow the runner to send keyboard
        // commands via TCP.
        "-monitor",
        "telnet:localhost:8888,server,nowait",
    ];

    if ctx.get_var("vga_console").is_some() {
        args.extend_from_slice(&["-vga", "std", "-display", "gtk"]);
    } else {
        args.extend_from_slice(&["-display", "none"]);
    }

    let qemu = "qemu-system-x86_64";
    let qemu = Command::new(qemu)
        .args(&args)
        .stdout::<std::fs::File>(ui.child_stdout(qemu)?)
        .stderr::<std::fs::File>(ui.child_stderr(qemu)?)
        .spawn()?;

    ui.set_substep("connecting to QEMU's telnet control interface");
    let mut attempts = 0;
    let mut telnet = loop {
        match std::net::TcpStream::connect("127.0.0.1:8888") {
            Ok(stream) => break Ok(stream),
            Err(_) => {
                if attempts < 10 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    attempts += 1;
                } else {
                    break Err(anyhow::anyhow!(
                        "timed out waiting for QEMU to start a telnet server"
                    ));
                }
            }
        }
    }?;

    // Simulate mashing the Enter key to get past the "Press any key to boot
    // from CD or DVD" prompt and the Windows boot menu.
    ui.set_substep("waiting for guest to complete installation");
    for _ in 0..20 {
        telnet.write_all("sendkey ret\n".as_bytes())?;
        telnet.flush()?;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    let output = qemu.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("QEMU returned non-success exit code: {:?}", output);
    }

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

fn get_script() -> Vec<ScriptStep> {
    vec![
        ScriptStep::with_prereqs(
            "create output image",
            create_output_image,
            &["qemu-img"],
        ),
        ScriptStep::with_prereqs(
            "create guest configuration ISO",
            create_config_iso,
            &["genisoimage"],
        ),
        ScriptStep::new(
            "copy unattend files to work directory",
            copy_unattend_files_to_work_dir,
        ),
        ScriptStep::new(
            "customize Autounattend.xml",
            customize_autounattend_xml,
        ),
        ScriptStep::with_prereqs(
            "install Windows to output image using QEMU",
            install_via_qemu,
            &["qemu-system-x86_64"],
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
    ]
}
