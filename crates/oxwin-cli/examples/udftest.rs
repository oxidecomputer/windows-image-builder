// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let mut img = oxwin_core::UdfImage::open(std::path::Path::new(&path))?;
    println!("label: {:?}", img.label());
    let entries = img.walk()?;
    println!("{} entries", entries.len());
    let dirs = entries.iter().filter(|e| e.is_dir).count();
    println!("{} dirs, {} files", dirs, entries.len() - dirs);
    let total: u64 = entries.iter().map(|e| e.size).sum();
    println!("total bytes: {total}");
    for e in entries.iter().filter(|e| e.size > 200_000_000) {
        println!("  BIG {} = {} bytes", e.path, e.size);
    }
    for want in ["sources/install.wim", "setup.exe", "efi/boot/bootx64.efi"] {
        match entries.iter().find(|e| e.path.eq_ignore_ascii_case(want)) {
            Some(e) => println!("  found {} ({} bytes)", e.path, e.size),
            None => println!("  MISSING {want}"),
        }
    }
    Ok(())
}
