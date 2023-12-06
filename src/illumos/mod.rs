// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Commands for creating a Windows installation disk and a generic Windows
//! image using illumos and propolis-standalone.
//!
//! Because Propolis doesn't support presenting virtual disks as removable disks
//! (at least at the time of this writing), the only way to inject an answer
//! file into the installation process is to stick it on the installation disk
//! proper. wimsy does this by creating a blank raw disk, partitioning it with a
//! GPT, and distributing the installation files into the partitions on the new
//! install disk: WinPE and all the unattend files go onto a FAT32 setup
//! partition, and the `install.wim` installable image goes onto a separate NTFS
//! partition.
//!
//! Once this disk is created, creating a generic Oxide Windows image is the
//! same as on other platforms: create a VM, attach a blank installation target
//! disk and the installation media, and then boot the VM; Windows Setup will
//! take care of the rest.

use crate::{app::Command, runner::Script};

use self::{
    build_installation_disk::{
        BuildInstallationDiskArgs, BuildInstallationDiskScript,
    },
    create_guest_disk_image::{
        CreateGuestDiskImageArgs, CreateGuestDiskImageScript,
    },
};

mod build_installation_disk;
mod create_guest_disk_image;

pub fn get_script(app: &crate::app::App) -> Box<dyn Script> {
    match &app.command {
        Command::BuildInstallationDisk { sources } => Box::new(
            BuildInstallationDiskScript::new(BuildInstallationDiskArgs {
                work_dir: app.work_dir.clone(),
                output_image: app.output_image.clone(),
                sources: sources.clone(),
            }),
        ),
        Command::CreateGuestDiskImage {
            vnic_link,
            installer_image,
            propolis_bootrom,
        } => Box::new(CreateGuestDiskImageScript::new(
            CreateGuestDiskImageArgs {
                work_dir: app.work_dir.clone(),
                output_image: app.output_image.clone(),
                vnic_link: vnic_link.clone(),
                installer_image: installer_image.clone(),
                propolis_bootrom: propolis_bootrom.clone(),
            },
        )),
    }
}
