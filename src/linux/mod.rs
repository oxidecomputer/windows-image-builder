// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Commands for creating a Windows guest image using QEMU.

use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand};

use crate::{autounattend::VirtioDriverVersion, runner::Script};

use self::create_guest_disk_image::CreateGuestDiskImageScript;

mod create_guest_disk_image;

#[derive(Parser)]
pub struct App {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    CreateGuestDiskImage {
        #[command(flatten)]
        args: CreateGuestDiskImageArgs,
    },
}

#[derive(Args, Clone)]
struct CreateGuestDiskImageArgs {
    /// The path to a directory to use for temporary files created by this
    /// workflow.
    #[arg(long)]
    work_dir: Utf8PathBuf,

    /// The path to the Windows installer ISO to repack into a customized
    /// installation disk.
    #[arg(long)]
    windows_iso: Utf8PathBuf,

    /// The path to an ISO containing signed virtio-net and virtio-block drivers
    /// to install. The drivers on this disk must have the directory structure
    /// the Fedora project uses in its virtio driver disks:
    ///
    /// - Top-level directories named `viostor` and `NetKVM`
    ///
    /// - Within each of these directories, subdirectories named `2k16`, `2k19`,
    ///   and `2k22`
    ///
    /// - Within each of these directories, an `amd64` subdirectory, which
    ///   contains `.cat`, `.inf`, and `.sys` files (i.e. the driver collateral
    ///   itself)
    #[arg(long)]
    virtio_iso: Utf8PathBuf,

    /// The path to a directory containing the unattend files to inject into the
    /// image.
    #[arg(long)]
    unattend_dir: Utf8PathBuf,

    /// The path to the OVMF bootrom to supply to QEMU for use as a guest
    /// firmware image.
    #[arg(long)]
    ovmf_path: Utf8PathBuf,

    /// An optional image index to write into the Microsoft-Windows-Setup
    /// component's ImageInstall/OSImage/InstallFrom elements in
    /// Autounattend.xml. This index determines the edition of Windows that will
    /// be installed when the installation media contains multiple editions
    /// (e.g. Server Standard, Standard with the Desktop Experience Pack, etc.).
    /// If not specified, the index in the Autounattend.xml specified by
    /// --unattend-dir is used.
    #[arg(long)]
    unattend_image_index: Option<u32>,

    /// An optional Windows Server version that specifies the driver
    /// installation paths to specify in Autounattend.xml. If set, this
    /// substitutes the appropriate versioned directory name ("2k16", "2k19", or
    /// "2k22") into the DriverPaths specified in the template Autounattend.xml
    /// specified by --unattend-dir. If not specified, the existing driver paths
    /// in that Autounattend.xml are used.
    #[arg(long, value_enum)]
    windows_version: Option<VirtioDriverVersion>,

    /// The path at which to create the repacked installation disk.
    #[arg(long)]
    output_image: Utf8PathBuf,
}

impl App {
    pub fn get_script(&self) -> Box<dyn Script> {
        match &self.command {
            Command::CreateGuestDiskImage { args } => {
                Box::new(CreateGuestDiskImageScript::new(args.clone()))
            }
        }
    }
}
