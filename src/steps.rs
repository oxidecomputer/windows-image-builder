// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common script steps that are shared between multiple OSes.

use std::process::Command;

use crate::util::{grep_command_for_row_and_column, run_command_check_status};

use anyhow::{Context as _, Result};

/// Uses `qemu-img` to create a blank output disk to which Windows can be
/// installed.
pub fn create_output_image(image_path: &str) -> Result<()> {
    run_command_check_status(
        Command::new("qemu-img")
            .args(["create", "-f", "raw", image_path, "30G"]),
    )
    .map(|_| ())
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
) -> Result<(String, String)> {
    let sector_size = grep_command_for_row_and_column(
        Command::new("sgdisk").args(["-p", image_path]),
        "Sector size",
        3,
    )
    .context("running 'sgdisk -p' to get sector size")?;

    let last_sector = grep_command_for_row_and_column(
        Command::new("sgdisk").args(["-i", "4", image_path]),
        "Last sector",
        2,
    )
    .context("running 'sgdisk -i' to get partition length in sectors")?;

    Ok((sector_size, last_sector))
}

/// Given an installed Windows image at `image_path` whose sector size is
/// `sector_size` and where the last sector of the last partition on the disk is
/// `last_sector`, trims unused sectors from the image, leaving just enough
/// space at the end to fit a new secondary GUID partition table.
pub fn shrink_output_image(
    image_path: &str,
    sector_size: &str,
    last_sector: &str,
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
    run_command_check_status(Command::new("qemu-img").args([
        "resize",
        "-f",
        "raw",
        image_path,
        &new_disk_size.to_string(),
    ]))
    .map(|_| ())
}

pub fn repair_secondary_gpt(image_path: &str) -> Result<()> {
    run_command_check_status(Command::new("sgdisk").args(["-e", image_path]))
        .map(|_| ())
}
