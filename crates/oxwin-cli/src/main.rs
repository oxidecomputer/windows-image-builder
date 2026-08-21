// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The CLI.
//!
//! Two commands:
//!
//! - `doctor` checks that everything the GUI depends on is actually present — useful
//!   before a demo, and useful for someone reporting a problem.
//! - `build` produces an image. Same engine the GUI uses, driven by flags instead of by
//!   a wizard, which is what makes it usable from a script and from a test harness.
//!
//! This file is allowed to print. `oxwin-core` is not — see DEVELOPMENT.md.

use anyhow::{Context, Result, bail};
use oxwin_core::builder::{self, Request};
use oxwin_core::engine::Cancel;
use oxwin_core::media::Media;
use oxwin_core::progress::{Event, Reporter};
use oxwin_core::settings::WindowsRelease;
use oxwin_core::unattend::Config;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("doctor");
    match cmd {
        "doctor" => doctor(),
        "build" => build(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => {
            eprintln!("unknown command {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

const USAGE: &str = "\
usage: oxwin doctor
       oxwin build <iso-or-mount> <out.img> [--opt=value]

  --name=<hostname>      computer name, or * for a golden image
  --user=<name>          local administrator to create
  --password=<secret>    its password. Required: SAC and RDP have no key auth
  --ssh-key=<a;b>        public keys authorised for SSH, semicolon separated
  --drivers=0            do not inject virtio drivers (diagnostic control:
                         isolates a hang in Setup from the drivers)
  --verbose-serial       extra OXIDE-STAGE markers on COM1, so a hang can be
                         localised to a pass rather than just observed
  --log-path=<path>      where Setup writes setupact.log/setuperr.log. Its
                         default is the WinPE RAM disk, so a stalled install
                         loses its own explanation on reset
  --no-ui-on-error       WillShowUI=Never instead of OnError. On a guest with
                         no console, OnError is an infinite hang
  --ssh=0                do not install OpenSSH in the guest
  --rdp=0                do not enable RDP
  --windows=<ws2019|ws2022|ws2025|win10|win11>
                         advisory: the media's own release overrides it
  --edition=<hint>       datacenter, standard, an index, or an EDITIONID;
                         omitted, the media's own list picks a sensible default
  --edition-hint=<hint>  overrides --edition when reading the WIM's image list
  --target-disk=<n>      disk index Setup installs to
  --product-key=<key>
  --ei-channel=<Eval|_Default|none>
  --bare                 media only: no answer file, no drivers, no ei.cfg
  --assets=<dir>         a payload directory, instead of the embedded one
  --quiet";

fn build(args: &[String]) -> Result<()> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [source, out] = positional.as_slice() else {
        bail!("build needs a source and an output path\n\n{USAGE}");
    };
    let opt = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let flag = |name: &str| args.iter().any(|a| a == &format!("--{name}"));

    let source = PathBuf::from(source);
    let media = if source.is_dir() {
        Media::Directory(source)
    } else {
        Media::Iso(source)
    };

    let release = match opt("windows").as_deref().unwrap_or("ws2022") {
        "ws2022" => WindowsRelease::Server2022,
        other => bail!("unknown --windows={other}; this build supports ws2022"),
    };
    // No default password anywhere in this workspace, and requiring it here is the
    // point: a password nobody chose is a password nobody changes.
    let password = opt("password")
        .context("--password is required: SAC and RDP have no SSH-key auth")?;

    let config = Config {
        release,
        edition: opt("edition").unwrap_or_default(),
        computer_name: opt("name").unwrap_or_else(|| "oxide-win".into()),
        username: opt("user").unwrap_or_else(|| "oxide".into()),
        password,
        enable_rdp: opt("rdp").as_deref() != Some("0"),
        inject_drivers: opt("drivers").as_deref() != Some("0"),
        enable_serial_console: true,
        target_disk: opt("target-disk")
            .as_deref()
            .unwrap_or("1")
            .parse()
            .context("--target-disk must be a disk index")?,
        locale: "en-US".into(),
        timezone: "UTC".into(),
        product_key: opt("product-key"),
        auto_logon: false,
        verbose_serial: flag("verbose-serial"),
        log_path: opt("log-path"),
        show_ui_on_error: !flag("no-ui-on-error"),
        image_index: None,
        skip_image_install: opt("image-install").as_deref() == Some("0"),
        install_from: None,
        install_from_letter: None,
        install_from_label: None,
        ssh_keys: opt("ssh-key")
            .unwrap_or_default()
            .split(';')
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect(),
        enable_ssh: opt("ssh").as_deref() != Some("0"),
    };

    // Embedded unless told otherwise. `--assets` beats `OXWIN_ASSETS` because an
    // explicit flag should win over an inherited environment.
    let assets = match opt("assets") {
        Some(dir) => oxwin_core::Assets::Directory(PathBuf::from(dir)),
        None => oxwin_core::Assets::discover(),
    };
    if let Some(problem) = assets.problem() {
        bail!("{problem}");
    }

    let request = Request {
        media,
        out: PathBuf::from(out),
        config,
        edition_hint: opt("edition-hint"),
        ei_channel: opt("ei-channel"),
        bare: flag("bare"),
        assets,
    };

    let quiet = flag("quiet");
    let (tx, rx) = std::sync::mpsc::channel();
    let printer = std::thread::spawn(move || {
        let mut last_percent = u8::MAX;
        for event in rx {
            match event {
                Event::Phase { name, message } => {
                    if !quiet {
                        println!("[{name}] {message}");
                    }
                }
                Event::Log(line) => {
                    if !quiet {
                        println!("{line}");
                    }
                }
                Event::Fraction { fraction, detail } => {
                    // One line per percent, not per chunk: this fires thousands of
                    // times over a 4 GiB copy and a terminal is not a progress bar.
                    let percent = (fraction * 100.0) as u8;
                    if !quiet && percent != last_percent {
                        println!("  copying {detail}: {percent}%");
                        last_percent = percent;
                    }
                }
                Event::Done { artifact, bytes } => {
                    println!(
                        "wrote {} ({:.2} GiB)",
                        artifact.display(),
                        bytes as f64 / (1024.0 * 1024.0 * 1024.0)
                    );
                }
                Event::Failed { message } => eprintln!("failed: {message}"),
            }
        }
    });

    let result = builder::build(&request, &Reporter::new(tx), &Cancel::new());
    printer.join().ok();
    let output = result?;
    if !quiet {
        println!(
            "image {} ({}), {} media files, {:.2} GiB copied",
            output.image_index,
            output.edition_id,
            output.media_files,
            output.copied_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        );
    }
    Ok(())
}

fn doctor() -> Result<()> {
    let mut failures = 0;

    // The payload is fetched by tools/fetch-payload.sh rather than committed and is
    // normally compiled in, so "this binary cannot build an image" is a build-time
    // mistake that has to be visible before someone tries.
    let assets = oxwin_core::Assets::discover();
    match assets.problem() {
        None => {
            println!("ok    payload: {}", assets.describe());
            // The three the boot partition cannot be built without. Read rather than
            // listed, so a truncated file fails here too.
            for name in ["shellx64.efi", "uefintfs.efi", "exfat_x64.efi"] {
                match assets.read(&format!("efi/{name}")) {
                    Ok(data) if !data.is_empty() => {
                        println!("ok    asset {name} ({} bytes)", data.len())
                    }
                    Ok(_) => {
                        println!("FAIL  asset {name} is empty");
                        failures += 1;
                    }
                    Err(e) => {
                        println!("FAIL  asset {name}: {e}");
                        failures += 1;
                    }
                }
            }
        }
        Some(problem) => {
            println!("FAIL  payload: {problem}");
            failures += 1;
        }
    }

    // Look it up on PATH rather than running it: the oxide CLI has no --version, and
    // every other subcommand either needs auth or does something.
    match find_on_path("oxide") {
        Some(p) => println!("ok    oxide CLI: {}", p.display()),
        // Not fatal: the app can still build and save an image without it.
        None => println!(
            "warn  oxide CLI not found — needed only to upload to a rack"
        ),
    }

    if failures > 0 {
        eprintln!("\n{failures} problem(s) found");
        std::process::exit(1);
    }
    println!("\neverything needed to build an image is present");
    Ok(())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|c| c.is_file())
}
