// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Write the app icon out as a PNG, at a size.
//!
//! ```
//! cargo run -p oxwin-gui --example icon-png -- icon_512x512.png 512
//! ```
//!
//! `tools/package-macos.sh` calls this once per size Apple's `iconutil` expects
//! and builds the bundle's `.icns` from the results, so the Dock icon and the
//! window icon are the same arithmetic rather than a `.png` committed next to the
//! source. `image` is a dev-dependency here and already in eframe's own tree, so
//! this costs the shipped binary nothing.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: icon-png <out.png> [edge]")?;
    let edge: u32 = match args.next() {
        Some(n) => n.parse()?,
        None => 512,
    };

    let rgba = oxwin_gui::icon_rgba(edge as usize);
    let image = image::RgbaImage::from_raw(edge, edge, rgba)
        .ok_or("the renderer returned a buffer of the wrong length")?;
    image.save(&path)?;
    println!("wrote {path} at {edge}x{edge}");
    Ok(())
}
