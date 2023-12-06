// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Commands for creating a Windows guest image using QEMU.

use crate::{app::Command, runner::Script};

use self::create_guest_disk_image::{
    CreateGuestDiskImageArgs, CreateGuestDiskImageScript,
};

mod create_guest_disk_image;

pub fn get_script(app: &crate::app::App) -> Box<dyn Script> {
    match &app.command {
        Command::CreateGuestDiskImage { sources, ovmf_path, vga_console } => {
            Box::new(CreateGuestDiskImageScript::new(
                CreateGuestDiskImageArgs {
                    sources: sources.clone(),
                    work_dir: app.work_dir.clone(),
                    output_image: app.output_image.clone(),
                    ovmf_path: ovmf_path.clone(),
                    vga_console: *vga_console,
                },
            ))
        }
    }
}
