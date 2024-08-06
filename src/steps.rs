// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common script steps that are shared between multiple OSes.

use std::process::Command;

use crate::{
    ui::Ui,
    util::{grep_command_for_row_and_column, run_command_check_status},
};

use anyhow::{Context as _, Result};

/// Uses `qemu-img` to create a blank output disk to which Windows can be
/// installed.
pub fn create_output_image(image_path: &str, ui: &dyn Ui) -> Result<()> {
    run_command_check_status(
        Command::new("qemu-img")
            .args(["create", "-f", "raw", image_path, "30G"]),
        ui,
    )
    .map(|_| ())
}

pub struct GptPartitionInformation {
    pub sector_size: String,
    pub first_sector: String,
    pub last_sector: String,
    pub partition_sectors: String,
}

/// Uses `sgdisk` to get the sector size, first and last sector offset, and
/// partition size (in sectors) for an arbitrary partition ID in the supplied
/// image.
pub fn get_gpt_partition_information(
    image_path: &str,
    partition_id: u32,
    ui: &dyn Ui,
) -> Result<GptPartitionInformation> {
    let partition_id_string = partition_id.to_string();
    let sector_size = grep_command_for_row_and_column(
        Command::new("sgdisk").args(["-p", image_path]),
        "Sector size",
        3,
        ui,
    )
    .context("running 'sgdisk -p' to get sector size")?;

    let first_sector = grep_command_for_row_and_column(
        Command::new("sgdisk").args(["-i", &partition_id_string, image_path]),
        "First sector",
        2,
        ui,
    )
    .context("getting first sector offset from 'sgdisk -i'")?;

    let last_sector = grep_command_for_row_and_column(
        Command::new("sgdisk").args(["-i", &partition_id_string, image_path]),
        "Last sector",
        2,
        ui,
    )
    .context("getting last sector offset from 'sgdisk -i'")?;

    let partition_sectors = grep_command_for_row_and_column(
        Command::new("sgdisk").args(["-i", &partition_id_string, image_path]),
        "Partition size",
        2,
        ui,
    )
    .context("getting partition sector count from 'sgdisk -i'")?;

    Ok(GptPartitionInformation {
        sector_size,
        first_sector,
        last_sector,
        partition_sectors,
    })
}

/// Uses `sgdisk` to get the sector size and the offset of the last sector in an
/// output image.
///
/// # Arguments
///
/// - image_path: The path to a Windows image that was produced by running the
///   Windows installer and attendant unattend scripts. The image is presumed to
///   have 4 partitions, the last of which is the main Windows OS partition;
///   running Windows setup with the unattend scripts in this repository will
///   produce an appropriately-partitioned disk.
///
/// # Return value
///
/// - `Ok(sector size, last sector)` if the relevant `sgdisk` commands
///   succeeded.
/// - `Err` if an `sgdisk` command failed or did not produce the expected
///   output.
pub fn get_output_image_partition_size(
    image_path: &str,
    ui: &dyn Ui,
) -> Result<(String, String)> {
    get_gpt_partition_information(image_path, 4, ui)
        .map(|info| (info.sector_size, info.last_sector))
}

/// Given an installed Windows image at `image_path` whose sector size is
/// `sector_size` and where the last sector of the last partition on the disk is
/// `last_sector`, trims unused sectors from the image, leaving just enough
/// space at the end to fit a new secondary GUID partition table.
pub fn shrink_output_image(
    image_path: &str,
    sector_size: &str,
    last_sector: &str,
    ui: &dyn Ui,
) -> Result<()> {
    let sector_size =
        sector_size.parse::<u64>().context("parsing sector size as u64")?;

    let last_sector = last_sector
        .parse::<u64>()
        .context("parsing last sector number as u64")?;

    let os_partition_size = sector_size * last_sector;

    // Leave 34 sectors after the last partition for the secondary GPT. Note
    // that this GPT won't exist in the truncated disk; the caller needs to
    // recreate it, e.g. using `sgdisk -e`.
    let new_disk_size = os_partition_size + (34 * sector_size);
    let new_disk_size = new_disk_size.to_string();

    // QEMU 5.10 and later require callers to pass the `--shrink` flag when
    // shrinking an image with `qemu-img resize`. This flag was added in QEMU
    // 2.11, so it's been around for a while, but it's not impossible for a
    // sufficiently old host not to have it (Debian 9's online manpages, for
    // example, don't include the flag, and the illumos /system/kvm package
    // installs a qemu-img binary that excludes it).
    //
    // To try to maximize compatibility, optimistically pass the `--shrink` flag
    // to start with. If that fails, fall back to running without `--shrink` to
    // see if that resolves the problem.
    let mut args =
        vec!["resize", "--shrink", "-f", "raw", image_path, &new_disk_size];
    if run_command_check_status(Command::new("qemu-img").args(&args), ui)
        .is_ok()
    {
        return Ok(());
    }

    // This will overwrite the log file output from the previous invocation, but
    // if this step fails, it's probably going to be for the same reason the
    // previous invocation did (i.e. something else is probably wrong that
    // isn't related to whether `--shrink` was used).
    assert_eq!(args.remove(1), "--shrink");
    run_command_check_status(Command::new("qemu-img").args(&args), ui)
        .map(|_| ())
}

pub fn repair_secondary_gpt(image_path: &str, ui: &dyn Ui) -> Result<()> {
    run_command_check_status(
        Command::new("sgdisk").args(["-e", image_path]),
        ui,
    )
    .map(|_| ())
}
