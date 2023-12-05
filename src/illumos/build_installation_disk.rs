// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{
    collections::HashMap,
    process::{Command, Stdio},
    str::FromStr,
};

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use itertools::iproduct;

use crate::{
    runner::{Context, Script, ScriptStep},
    steps::get_gpt_partition_information,
    util::run_command_check_status,
};

use super::BuildInstallationDiskArgs;

const UNATTEND_FILES: &[&'static str] = &[
    "Autounattend.xml",
    "cloudbase-init-unattend.conf",
    "cloudbase-init.conf",
    "OxidePrepBaseImage.ps1",
    "prep.cmd",
    "specialize-unattend.xml",
];

pub struct BuildInstallationDiskScript {
    steps: Vec<ScriptStep>,
    args: BuildInstallationDiskArgs,
}

impl BuildInstallationDiskScript {
    pub(super) fn new(script_args: BuildInstallationDiskArgs) -> Self {
        Self { steps: get_script(), args: script_args }
    }
}

impl Script for BuildInstallationDiskScript {
    fn steps(&self) -> &[ScriptStep] {
        self.steps.as_slice()
    }

    fn file_prerequisites(&self) -> Vec<Utf8PathBuf> {
        let mut prereqs = self.args.file_prerequisites();
        for file in UNATTEND_FILES {
            let mut path = self.args.unattend_dir.clone();
            path.push(file);
            prereqs.push(path);
        }

        prereqs
    }

    fn initial_context(&self) -> HashMap<String, String> {
        let args = &self.args;

        let mut ctx: HashMap<String, String> = [
            ("work_dir".to_string(), args.work_dir.to_string()),
            ("windows_iso".to_string(), args.windows_iso.to_string()),
            ("virtio_iso".to_string(), args.virtio_iso.to_string()),
            ("unattend_dir".to_string(), args.unattend_dir.to_string()),
            ("output_image".to_string(), args.output_image.to_string()),
        ]
        .into_iter()
        .collect();

        if let Some(image_index) = args.unattend_image_index {
            ctx.insert(
                "unattend_image_index".to_string(),
                image_index.to_string(),
            );
        }

        if let Some(windows_version) = args.windows_version {
            ctx.insert(
                "windows_version".to_string(),
                windows_version.as_driver_path_component().to_string(),
            );
        } else {
            ctx.insert("windows_version".to_string(), "2k22".to_string());
        }

        ctx
    }
}

fn create_installer_disk(ctx: &mut Context) -> Result<()> {
    run_command_check_status(Command::new("qemu-img").args([
        "create",
        "-f",
        "raw",
        ctx.get_var("output_image").unwrap(),
        // The disk needs to be large enough so that its entire size less 1 GiB
        // (the size of the WinPE partition) is large enough to hold an
        // arbitrary install.wim. 7 GiB is enough headroom for Server 2016 and
        // Server 2022.
        "8G",
    ]))
    .map(|_| ())
}

fn set_up_installer_gpt_table(ctx: &mut Context) -> Result<()> {
    run_command_check_status(
        Command::new("sgdisk")
            .args(["-og", ctx.get_var("output_image").unwrap()]),
    )
    .map(|_| ())
}

fn create_installer_disk_partitions(ctx: &mut Context) -> Result<()> {
    run_command_check_status(Command::new("sgdisk").args([
        "-n=1:0:+1G",
        "-t",
        "1:0700",
        "-n=2:0:0",
        "-t",
        "2:0700",
        ctx.get_var("output_image").unwrap(),
    ]))
    .map(|_| ())
}

fn set_installer_disk_partition_ids(ctx: &mut Context) -> Result<()> {
    // N.B. These partition GUIDs must match the GUIDs in
    // Autounattend.xml.
    const PARTITION_GUID_1: &str = "569CBD84-352D-44D9-B92D-BF25B852925B";
    const PARTITION_GUID_2: &str = "A94E24F7-92C9-405C-82AA-9A1B45BA180C";

    run_command_check_status(Command::new("sgdisk").args([
        "-u",
        &format!("1:{PARTITION_GUID_1}"),
        "-u",
        &format!("2:{PARTITION_GUID_2}"),
        ctx.get_var("output_image").unwrap(),
    ]))
    .map(|_| ())
}

fn mount_installer_disk_as_loopback_device(ctx: &mut Context) -> Result<()> {
    let repack_loop = run_command_check_status(Command::new("pfexec").args([
        "lofiadm",
        "-l",
        "-a",
        ctx.get_var("output_image").unwrap(),
    ]))?;

    // `lofiadm` returns a path to a partition on the loopback disk device.
    // Subsequent commands want to operate on slices instead. Compute the
    // relevant slice paths and stash them in the context.
    let repack_loop = String::from_utf8_lossy(&repack_loop.stdout).to_string();
    let block_device = repack_loop
        .strip_prefix("/dev/dsk/")
        .ok_or(anyhow::anyhow!("loopback device not mounted under /dev/dsk"))?
        .trim_end()
        .strip_suffix("p0")
        .ok_or(anyhow::anyhow!(
            "loopback device path does not end in partition ID 'p0'"
        ))?;

    ctx.set_var("repack_loop", repack_loop.trim_end().to_string());
    ctx.set_var(
        "repack_loop_setup_raw",
        format!("/dev/rdsk/{}s0", block_device),
    );
    ctx.set_var("repack_loop_setup", format!("/dev/dsk/{}s0", block_device));
    ctx.set_var("repack_loop_image", format!("/dev/dsk/{}s1", block_device));

    Ok(())
}

fn create_winpe_fat32(ctx: &mut Context) -> Result<()> {
    let yes_cmd = Command::new("yes").stdout(Stdio::piped()).spawn()?;
    run_command_check_status(
        Command::new("pfexec")
            .args([
                "mkfs",
                "-F",
                "pcfs",
                "-o",
                "fat=32",
                &ctx.get_var("repack_loop_setup_raw").unwrap(),
            ])
            .stdin(Stdio::from(yes_cmd.stdout.ok_or(anyhow::anyhow!(
                "failed to get stdout from 'yes' to pipe to 'mkfs'"
            ))?)),
    )
    .map(|_| ())
}

fn mount_winpe_partition(ctx: &mut Context) -> Result<()> {
    let mut setup_mount =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();

    setup_mount.push("setup-mount");
    std::fs::create_dir_all(&setup_mount)
        .context("mounting WinPE partition")?;

    run_command_check_status(Command::new("pfexec").args([
        "mount",
        "-F",
        "pcfs",
        &ctx.get_var("repack_loop_setup").unwrap(),
        setup_mount.as_str(),
    ]))?;

    ctx.set_var("setup_mount", setup_mount.to_string());
    Ok(())
}

fn extract_setup_to_winpe_partition(ctx: &mut Context) -> Result<()> {
    // This is an expensive operation, so make `7z` inherit the runner's stdout
    // so that progress displays in the terminal.
    run_command_check_status(
        Command::new("7z")
            .args([
                "x",
                "-x!sources/install.wim",
                ctx.get_var("windows_iso").unwrap(),
                &format!("-o{}", &ctx.get_var("setup_mount").unwrap()),
            ])
            .stdout(Stdio::inherit()),
    )
    .map(|_| ())
}

fn copy_unattend_files_to_work_dir(ctx: &mut Context) -> Result<()> {
    let mut work_unattend =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();
    work_unattend.push("unattend");
    std::fs::create_dir_all(&work_unattend)
        .context("creating temporary directory for unattend files")?;

    let unattend_dir =
        Utf8PathBuf::from_str(ctx.get_var("unattend_dir").unwrap()).unwrap();

    for filename in UNATTEND_FILES {
        let mut src = unattend_dir.clone();
        src.push(filename);
        let mut dst = work_unattend.clone();
        dst.push(filename);
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copying {src} to {dst}"))?;
    }

    // Make subsequent steps use unattend files from
    ctx.set_var("unattend_dir", work_unattend.to_string());
    Ok(())
}

fn customize_autounattend_xml(ctx: &mut Context) -> Result<()> {
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

fn copy_unattend_to_winpe_partition(ctx: &mut Context) -> Result<()> {
    let setup_mount = ctx.get_var("setup_mount").unwrap();
    let unattend_dir =
        Utf8PathBuf::from_str(ctx.get_var("unattend_dir").unwrap()).unwrap();

    for filename in [
        "Autounattend.xml",
        "OxidePrepBaseImage.ps1",
        "prep.cmd",
        "specialize-unattend.xml",
    ] {
        println!("  copying {filename} to WinPE partition");
        let mut unattend = unattend_dir.clone();
        unattend.push(filename);
        if !unattend.exists() {
            anyhow::bail!("{filename} not found in unattend directory");
        }

        let mut dst = Utf8PathBuf::from_str(setup_mount).unwrap();
        dst.push(filename);
        std::fs::copy(&unattend, &dst).with_context(|| {
            format!("copying {filename} to WinPE partition")
        })?;
    }

    Ok(())
}

fn copy_cloudbase_init_to_winpe_partition(ctx: &mut Context) -> Result<()> {
    let unattend_dir =
        Utf8PathBuf::from_str(ctx.get_var("unattend_dir").unwrap()).unwrap();
    let mut cloudbase_dir =
        Utf8PathBuf::from_str(ctx.get_var("setup_mount").unwrap()).unwrap();
    cloudbase_dir.push("cloudbase-init");
    std::fs::create_dir_all(&cloudbase_dir)
        .context("creating cloudbase-init directory in WinPE partition")?;
    for filename in ["cloudbase-init-unattend.conf", "cloudbase-init.conf"] {
        let mut unattend = unattend_dir.clone();
        unattend.push(filename);
        let mut dst = cloudbase_dir.clone();
        dst.push(filename);
        std::fs::copy(&unattend, &dst).with_context(|| {
            format!("copying {filename} to WinPE partition")
        })?;
    }

    Ok(())
}

fn copy_virtio_to_winpe_partition(ctx: &mut Context) -> Result<()> {
    let setup_mount = ctx.get_var("setup_mount").unwrap();
    for (driver, ext) in iproduct!(["viostor", "NetKVM"], ["cat", "inf", "sys"])
    {
        run_command_check_status(
            Command::new("7z")
                .args([
                    "e",
                    ctx.get_var("virtio_iso").unwrap(),
                    &format!("-o{}/virtio-drivers/", setup_mount),
                    &format!(
                        "{driver}/{}/amd64/*.{ext}",
                        ctx.get_var("windows_version").unwrap()
                    ),
                ])
                .stdout(Stdio::inherit()),
        )?;
    }
    Ok(())
}

fn unmount_winpe_partition(ctx: &mut Context) -> Result<()> {
    let setup_mount = ctx.get_var("setup_mount").unwrap();
    run_command_check_status(
        Command::new("pfexec").args(["umount", setup_mount]),
    )
    .map(|_| ())
}

fn get_wim_partition_parameters(ctx: &mut Context) -> Result<()> {
    let params =
        get_gpt_partition_information(ctx.get_var("output_image").unwrap(), 2)?;

    ctx.set_var("sector_size", params.sector_size);
    ctx.set_var("first_sector", params.first_sector);
    ctx.set_var("partition_sectors", params.partition_sectors);

    Ok(())
}

fn create_wim_partition_ntfs(ctx: &mut Context) -> Result<()> {
    let sector_size = ctx.get_var("sector_size").unwrap();
    let first_sector = ctx.get_var("first_sector").unwrap();
    let partition_sectors = ctx.get_var("partition_sectors").unwrap();
    let repack_loop_image = ctx.get_var("repack_loop_image").unwrap();

    run_command_check_status(Command::new("pfexec").args([
        "mkntfs",
        "-Q",
        "-s",
        sector_size,
        "-p",
        first_sector,
        "-H",
        "16",
        "-S",
        "63",
        repack_loop_image,
        partition_sectors,
    ]))
    .map(|_| ())
}

fn mount_wim_partition(ctx: &mut Context) -> Result<()> {
    let mut image_mount =
        Utf8PathBuf::from_str(ctx.get_var("work_dir").unwrap()).unwrap();
    image_mount.push("image-mount");
    std::fs::create_dir_all(&image_mount)
        .context("creating mount point for WIM partition")?;

    let repack_loop_image = ctx.get_var("repack_loop_image").unwrap();

    run_command_check_status(Command::new("pfexec").args([
        "ntfs-3g",
        repack_loop_image,
        image_mount.as_str(),
    ]))
    .map(|_| ())?;

    ctx.set_var("image_mount", image_mount.to_string());
    Ok(())
}

fn copy_install_wim(ctx: &mut Context) -> Result<()> {
    run_command_check_status(
        Command::new("7z")
            .args([
                "e",
                "-i!sources/install.wim",
                ctx.get_var("windows_iso").unwrap(),
                &format!("-o{}", ctx.get_var("image_mount").unwrap()),
            ])
            .stdout(Stdio::inherit()),
    )
    .map(|_| ())
}

fn unmount_wim_partition(ctx: &mut Context) -> Result<()> {
    run_command_check_status(
        Command::new("pfexec")
            .args(["umount", ctx.get_var("image_mount").unwrap()]),
    )
    .map(|_| ())
}

fn remove_loopback_device(ctx: &mut Context) -> Result<()> {
    // Sleep briefly to ensure the sync finishes before trying to remove the
    // device.
    std::thread::sleep(std::time::Duration::from_secs(2));
    run_command_check_status(Command::new("pfexec").args([
        "lofiadm",
        "-d",
        ctx.get_var("repack_loop").unwrap(),
    ]))
    .map(|_| ())
}

fn get_script() -> Vec<ScriptStep> {
    let steps = vec![
        ScriptStep::with_prereqs(
            "create new disk to hold installer image",
            create_installer_disk,
            &["qemu-img"],
        ),
        ScriptStep::with_prereqs(
            "set up GPT partition table on installer disk",
            set_up_installer_gpt_table,
            &["sgdisk"],
        ),
        ScriptStep::with_prereqs(
            "create partitions on installer disk",
            create_installer_disk_partitions,
            &["sgdisk"],
        ),
        ScriptStep::with_prereqs(
            "set partition IDs for partitions on installer disk",
            set_installer_disk_partition_ids,
            &["sgdisk"],
        ),
        ScriptStep::new(
            "mount installation image as loopback device",
            mount_installer_disk_as_loopback_device,
        ),
        ScriptStep::new(
            "create FAT32 filesystem on WinPE partition",
            create_winpe_fat32,
        ),
        ScriptStep::new("mount WinPE partition", mount_winpe_partition),
        ScriptStep::with_prereqs(
            "extract setup files to WinPE partition",
            extract_setup_to_winpe_partition,
            &["7z"],
        ),
        ScriptStep::new(
            "copy unattend files to working directory",
            copy_unattend_files_to_work_dir,
        ),
        ScriptStep::new(
            "customizing Autounattend.xml",
            customize_autounattend_xml,
        ),
        ScriptStep::new(
            "copying unattend scripts to WinPE partition",
            copy_unattend_to_winpe_partition,
        ),
        ScriptStep::new(
            "copying cloudbase-init scripts to WinPE partition",
            copy_cloudbase_init_to_winpe_partition,
        ),
        ScriptStep::with_prereqs(
            "copying virtio drivers to WinPE partition",
            copy_virtio_to_winpe_partition,
            &["7z"],
        ),
        ScriptStep::new("unmounting WinPE partition", unmount_winpe_partition),
        ScriptStep::with_prereqs(
            "reading partition parameters for WIM partition",
            get_wim_partition_parameters,
            &["sgdisk"],
        ),
        ScriptStep::with_prereqs(
            "creating NTFS filesystem on WIM partition",
            create_wim_partition_ntfs,
            &["mkntfs"],
        ),
        ScriptStep::with_prereqs(
            "mounting WIM partition",
            mount_wim_partition,
            &["ntfs-3g"],
        ),
        ScriptStep::with_prereqs(
            "unpacking install.wim into WIM partition",
            copy_install_wim,
            &["7z"],
        ),
        ScriptStep::new("unmounting image partition", unmount_wim_partition),
        ScriptStep::new("flushing changes to disk", |_ctx| {
            let mut sync = Command::new("sync");
            run_command_check_status(&mut sync).map(|_| ())
        }),
        ScriptStep::new("remove loopback device", remove_loopback_device),
    ];

    steps
}
