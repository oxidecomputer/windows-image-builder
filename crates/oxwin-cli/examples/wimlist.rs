// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Get contents of ISO, and the WIM inside.
//!
//! Prints everything detection reads off a piece of media: the architecture, the release
//! and how confidently it was identified, every installable image, and whether the media
//! ships an `ei.cfg`. Those are the fields [TESTED-MEDIA.md] identifies media by, so this
//! is what to run before adding a row, filenames vary, depending on the image provided.
//!
//! ```text
//! cargo run --release -p oxwin-cli --example wimlist -- ~/Storage/ISOs/*.iso
//! ```
//!
//! [TESTED-MEDIA.md]: https://github.com/oxidecomputer/windows-image-builder

use oxwin_core::media::{self, Media};
use oxwin_core::wim;

fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: wimlist <iso-or-mount>...");
        std::process::exit(2);
    }
    for path in paths {
        println!("\n== {path}");
        if let Err(e) = dump(&path) {
            println!("   FAILED: {e:#}");
        }
    }
    Ok(())
}

fn dump(path: &str) -> anyhow::Result<()> {
    let info = media::inspect(&Media::at(path))?;

    // The cheapest identity field there is: reportable without hashing 5 GB, and it is
    // what distinguishes two ISOs of the same release from each other.
    println!("   install.wim  {} bytes", info.wim_size);
    println!(
        "   arch         {}",
        match info.arch {
            Some(wim::ARCH_AMD64) => "9 (amd64)".to_string(),
            Some(wim::ARCH_ARM64) => "12 (arm64 — NOT SUPPORTED)".to_string(),
            Some(other) => format!("{other} (unknown)"),
            None => "-- absent --".to_string(),
        }
    );
    println!(
        "   product type {}   build {}",
        if info.product_type.is_empty() {
            "-- absent --"
        } else {
            &info.product_type
        },
        info.build.map(|b| b.to_string()).unwrap_or("--".into()),
    );
    println!(
        "   release      {}{}",
        info.release.map(|r| r.label()).unwrap_or("-- undetermined --"),
        if info.release.is_some() && !info.build_recognised {
            "  (guessed: this build is not in the release table)"
        } else {
            ""
        },
    );
    println!(
        "   channel      {}",
        if info.is_evaluation() { "evaluation" } else { "retail / volume" }
    );
    println!(
        "   ei.cfg       {}",
        info.ei_cfg.as_deref().unwrap_or("-- absent --")
    );

    println!("   {} image(s):", info.images.len());
    for image in &info.images {
        println!(
            "   [{:>2}] {:<44} editionId={:<26} {}",
            image.index,
            image.name,
            image.edition_id,
            if image.installation_type.is_empty() {
                "?"
            } else {
                &image.installation_type
            },
        );
    }

    for problem in info.problems() {
        println!(
            "   {} {}",
            if problem.blocking { "REFUSED:" } else { "warning:" },
            problem.message
        );
    }
    Ok(())
}
