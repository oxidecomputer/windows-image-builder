// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Defines a script for building a Windows guest image on a Linux system using
//! QEMU.

use std::{io::Write, process::Command, str::FromStr};

use anyhow::Result;
use camino::Utf8PathBuf;

use crate::{
    app::ImageSources,
    runner::{Context, Script, ScriptStep},
    util::run_command_check_status,
};

pub struct CreateGuestDiskImageArgs {
    pub work_dir: Utf8PathBuf,
    pub output_image: Utf8PathBuf,
    pub sources: ImageSources,
    pub ovmf_path: Utf8PathBuf,
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

    fn file_prerequisites(&self) -> Vec<camino::Utf8PathBuf> {
        let mut files = vec![
            self.args.sources.windows_iso.clone(),
            self.args.sources.virtio_iso.clone(),
            self.args.ovmf_path.clone(),
        ];

        for file in [
            "cloudbase-init.conf",
            "cloudbase-init-unattend.conf",
            "cloudbase-unattend.xml",
        ] {
            let mut path = self.args.sources.unattend_dir.clone();
            path.push(file);
            files.push(path);
        }

        files
    }

    fn initial_context(&self) -> std::collections::HashMap<String, String> {
        let args = &self.args;
        [
            ("work_dir".to_string(), args.work_dir.to_string()),
            ("windows_iso".to_string(), args.sources.windows_iso.to_string()),
            ("virtio_iso".to_string(), args.sources.virtio_iso.to_string()),
            ("unattend_dir".to_string(), args.sources.unattend_dir.to_string()),
            ("output_image".to_string(), args.output_image.to_string()),
            ("ovmf_path".to_string(), args.ovmf_path.to_string()),
        ]
        .into_iter()
        .collect()
    }
}

fn create_output_image(ctx: &mut Context) -> Result<()> {
    crate::steps::create_output_image(ctx.get_var("output_image").unwrap())
}

fn create_config_iso(ctx: &mut Context) -> Result<()> {
    let mut unattend_iso =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();
    unattend_iso.push("unattend.iso");
    run_command_check_status(Command::new("genisoimage").args([
        "-J",
        "-R",
        "-o",
        unattend_iso.as_str(),
        ctx.get_var("unattend_dir").unwrap(),
    ]))?;

    ctx.set_var("unattend_iso", unattend_iso.to_string());
    Ok(())
}

fn install_via_qemu(ctx: &mut Context) -> Result<()> {
    // Launch a VM in QEMU with the installation target disk attached as an NVMe
    // drive and CD-ROM drives containing the Windows installation media, the
    // virtio driver disk, and the answer file ISO created previously. Windows
    // setup will detect the presence of the answer file ISO and use the
    // Autounattend.xml located there to drive installation.
    let qemu = Command::new("qemu-system-x86_64")
        .args([
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
            &format!(
                "if=pflash,format=raw,readonly=on,file={}",
                ctx.get_var("ovmf_path").unwrap()
            ),
            "-netdev",
            "user,id=net0",
            "-device",
            "virtio-net-pci,netdev=net0",
            "-device",
            "nvme,drive=drivec,serial=01de01de,physical_block_size=512,\
                logical_block_size=512,discard_granularity=512",
            "-drive",
            &format!(
                "if=none,id=drivec,file={},format=raw",
                ctx.get_var("output_image").unwrap()
            ),
            "-device",
            "ide-cd,drive=win-disk,id=cd-disk0,unit=0,bus=ide.0",
            "-drive",
            &format!(
                "file={},if=none,id=win-disk,media=cdrom",
                ctx.get_var("windows_iso").unwrap()
            ),
            "-device",
            "ide-cd,drive=virtio-disk,id=cd-disk1,unit=0,bus=ide.1",
            "-drive",
            &format!(
                "file={},if=none,id=virtio-disk,media=cdrom",
                ctx.get_var("virtio_iso").unwrap()
            ),
            "-device",
            "ide-cd,drive=unattend-disk,id=cd-disk2,unit=1,bus=ide.0",
            "-drive",
            &format!(
                "file={},if=none,id=unattend-disk,media=cdrom",
                ctx.get_var("unattend_iso").unwrap()
            ),
            // Send serial console output to stdout so that the user can monitor
            // the installation's progress (the guest is configured to print
            // setup progress to COM1).
            "-serial",
            "stdio",
            // Set up the QEMU monitor to allow the runner to send keyboard
            // commands via TCP.
            "-monitor",
            "telnet:localhost:8888,server,nowait",
            "-display",
            "none",
            /*
            "vnc=127.0.0.1:0",
            "-vga",
            "std",
            */
        ])
        .spawn()?;

    println!("  Waiting for QEMU to open its telnet port");
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

fn get_partition_size(ctx: &mut Context) -> Result<()> {
    let (sector_size, last_sector) =
        crate::steps::get_output_image_partition_size(
            ctx.get_var("output_image").unwrap(),
        )?;

    ctx.set_var("sector_size", sector_size);
    ctx.set_var("last_sector", last_sector);
    Ok(())
}

fn shrink_output_image(ctx: &mut Context) -> Result<()> {
    crate::steps::shrink_output_image(
        ctx.get_var("output_image").unwrap(),
        ctx.get_var("sector_size").unwrap(),
        ctx.get_var("last_sector").unwrap(),
    )
}

fn repair_secondary_gpt(ctx: &mut Context) -> Result<()> {
    crate::steps::repair_secondary_gpt(ctx.get_var("output_image").unwrap())
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
        // TODO(gjc) copy unattend files to a temporary directory, then apply
        // autounattend fixups
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
