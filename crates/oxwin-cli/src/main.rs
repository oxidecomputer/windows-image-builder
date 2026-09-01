// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The CLI.
//!
//! Two commands:
//!
//! - `doctor` checks that everything the GUI depends on is actually present — useful
//!   before a demo, and useful for someone reporting a problem. That includes which
//!   racks this machine is logged into, and whether those logins have expired.
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
        "upload" => upload(&args[1..]),
        "instance" => instance(&args[1..]),
        "watch" => watch(&args[1..]),
        "snapshot" => snapshot(&args[1..]),
        "image" => image(&args[1..]),
        "teardown" => teardown(&args[1..]),
        "golden" => golden(&args[1..]),
        "verify" => verify(&args[1..]),
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
       oxwin upload <image.img> --project=<p> --disk=<name> [--opt=value]
       oxwin instance <name> --project=<p> --installer-disk=<d> [--opt=value]
       oxwin watch <instance> --project=<p> [--timeout=2h] [--poll=15s]
       oxwin snapshot <run> --project=<p>
       oxwin image <run> --project=<p> [--image-version=<v>]
       oxwin teardown <run> --project=<p> [--keep=image]
       oxwin golden <iso-or-mount-or-img> --run=<name> --project=<p>
       oxwin verify <run> --project=<p>

  --name=<hostname>      computer name, or * for a golden image
  --generalize           after the install finishes, sysprep /generalize and
                         shut down, so the disk can be cloned. Implied by
                         --name=*. Runs from a SYSTEM task at startup, so it
                         needs no autologon
  --user=<name>          local administrator to create
  --password-file=<path> file whose first line is the account's password. A
                         password is required: SAC and RDP have no key auth
  --password=<secret>    the same, on the command line -- readable by anything
                         that can run `ps`, so it warns. OXWIN_PASSWORD works
                         too and is not in `ps`
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
  --quiet

upload options:
  --project=<name>       project to create the disk in. Required
  --disk=<name>          name for the new disk. Required
  --profile=<name>       which `oxide auth login` profile to use. Omitted, the
                         SDK resolves OXIDE_TOKEN, then OXIDE_PROFILE, then the
                         default profile — naming one here disables OXIDE_TOKEN
  --description=<text>   disk description
  --block-size=<n>       512 (default), 2048 or 4096. Leave it alone for an
                         installer image: the MBR lays partitions out in
                         512-byte sectors, and any other value points them at
                         nothing

instance options:
  --project=<name>       project to build in. Required
  --installer-disk=<d>   the uploaded installer. Pinned as the boot disk, because
                         the control plane owns boot order and there is no
                         fallthrough. Required
  --profile=<name>       as for upload
  --cpus=<n>             vCPUs (default 4)
  --memory-gib=<n>       RAM in GiB (default 8)
  --system-disk=<name>   blank disk to install onto (default <name>-system)
  --system-disk-gib=<n>  its size (default 100)
  --no-start             create it stopped

watch options:
  --project=<name>       project the instance is in. Required
  --timeout=<dur>        give up after this long (default 2h). Server 2022
                         takes tens of minutes across several reboots, and
                         2025 and 11 are slower
  --poll=<dur>           how often to look (default 15s)
  --profile=<name>       as for upload

snapshot/image/teardown options:
  --project=<name>       Required
  --profile=<name>       as for upload
  --image-version=<v>    what the finished image reports as its version
  --os=<name>            and its OS family (default windows)
  --keep=<level>         image (default), snapshot, disks or all. Cumulative:
                         `snapshot` keeps the image too. There is no `none` --
                         the image is what the run is for

  These take a run name, not a resource name: every resource is derived from
  it, as <run>-installer, <run>-system, the instance <run>, <run>-snap and
  the image <run>. All three are idempotent, so re-running one is safe.

  A golden build finishes by shutting itself down, so the signal is the
  instance reaching `stopped`: a guest shutdown stops the instance and a
  guest reboot does not. Port 22 is polled alongside it for one judgement —
  stopping without it ever having answered means Setup never finished.

golden options:
  --run=<name>           names every resource, and is the whole of the state
                         this keeps. Re-running the identical command resumes
                         where it stopped: each step asks the rack what is
                         already there. Required
  --project=<name>       Required
  --image-out=<path>     where to write the built image (default <run>.img)
  --keep=<level>         as for teardown
  --timeout=<dur>        how long to wait for the install (default 2h)
  --image-version=<v>    detected from the media unless the source is an .img
  --system-disk-gib=<n>  size of the disk Windows installs onto (default 100)
  --verify-clone         when the image is made, create an instance from it
                         and prove it comes up and stays up. Leaves it
                         running: it is the proof. Same as `oxwin verify`
  --cpus=<n>  --memory-gib=<n>  --profile=<name>
                         as for instance, plus every build option above

  Given an ISO or a mount, golden builds the media itself and sets the
  golden-image options rather than trusting a flag: media built without them
  installs perfectly and never shuts down, which the watcher cannot tell
  apart from a hang. Given an .img it cannot know, so build that with
  --name=* or --generalize.";

/// Where the password may come from, in order of precedence.
///
/// **`--password=` puts the secret in the process's command line, which every other
/// process on the machine can read out of `ps`.** That is not hypothetical: it was
/// seen in the process table of the machine driving a real rack run. The flag stays,
/// because scripts and habits depend on it and removing it would break them silently,
/// but it warns, and there are now two ways to avoid it.
///
/// No default, ever: a password nobody chose is a password nobody changes, and
/// `Settings::default()` is deliberately unbuildable for the same reason.
#[derive(Debug, PartialEq, Eq)]
enum PasswordSource {
    /// `--password=`. Visible in `ps`.
    Flag(String),
    /// `--password-file=`. The file's first line.
    File(String),
    /// `OXWIN_PASSWORD`. Not in `ps`, though still in the environment.
    Environment(String),
}

/// Pick the source, without reading anything. Separated so the precedence is
/// testable without a filesystem or an environment.
fn choose_password(
    flag: Option<String>,
    file: Option<String>,
    env: Option<String>,
) -> Result<PasswordSource> {
    match (flag, file, env) {
        // An explicit flag wins over an inherited environment, the same rule
        // `--assets` follows.
        (Some(p), _, _) => Ok(PasswordSource::Flag(p)),
        (None, Some(f), _) => Ok(PasswordSource::File(f)),
        (None, None, Some(p)) => Ok(PasswordSource::Environment(p)),
        (None, None, None) => bail!(
            "a password is required: SAC and RDP have no SSH-key auth, so an \
             account with no password is reachable over SSH and nowhere else -- \
             including from the serial console, which is the one way in when \
             something has gone wrong.\n\n\
             Give it as --password-file=<path>, or in OXWIN_PASSWORD, or as \
             --password=<secret> -- though that last one is readable by anything \
             that can run `ps`."
        ),
    }
}

fn password_from_args(args: &[String]) -> Result<String> {
    let opt = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    match choose_password(
        opt("password"),
        opt("password-file"),
        std::env::var("OXWIN_PASSWORD").ok().filter(|v| !v.is_empty()),
    )? {
        PasswordSource::Flag(p) => {
            eprintln!(
                "warn  --password= is visible to anything that can run `ps`. \
                 Prefer --password-file= or OXWIN_PASSWORD."
            );
            Ok(p)
        }
        PasswordSource::Environment(p) => Ok(p),
        PasswordSource::File(path) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading the password from {path}"))?;
            // The first line, trimmed of its newline only: a password may legally
            // begin or end with a space, and silently trimming one produces a
            // machine nobody can log into for a reason nobody can see.
            let first = text.split('\n').next().unwrap_or_default();
            let password =
                first.strip_suffix('\r').unwrap_or(first).to_string();
            if password.is_empty() {
                bail!("{path} is empty, so there is no password in it");
            }
            Ok(password)
        }
    }
}

/// The answer-file config, from flags.
///
/// Shared by `build` and `golden` so the two cannot drift: `golden` differs only in
/// forcing the golden-image options, and duplicating forty lines to express that
/// would be an invitation for one copy to gain an option the other lacks.
fn config_from_args(args: &[String]) -> Result<Config> {
    let opt = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let flag = |name: &str| args.iter().any(|a| a == &format!("--{name}"));

    let release = match opt("windows").as_deref().unwrap_or("ws2022") {
        "ws2022" => WindowsRelease::Server2022,
        other => bail!("unknown --windows={other}; this build supports ws2022"),
    };
    let password = password_from_args(args)?;

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
        generalize: flag("generalize") || opt("name").as_deref() == Some("*"),
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
    Ok(config)
}

/// The payload, embedded unless told otherwise.
///
/// `--assets` beats `OXWIN_ASSETS` because an explicit flag should win over an
/// inherited environment.
fn assets_from_args(args: &[String]) -> Result<oxwin_core::Assets> {
    let dir = args.iter().find_map(|a| a.strip_prefix("--assets="));
    let assets = match dir {
        Some(dir) => oxwin_core::Assets::Directory(PathBuf::from(dir)),
        None => oxwin_core::Assets::discover(),
    };
    if let Some(problem) = assets.problem() {
        bail!("{problem}");
    }
    Ok(assets)
}

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

    let config = config_from_args(args)?;
    let assets = assets_from_args(args)?;

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
    let (reporter, printer) = printer(quiet, "copying");

    let result = builder::build(&request, &reporter, &cancel_on_interrupt());
    drop(reporter);
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

/// A thread that renders `progress::Event` as lines.
///
/// `verb` names what we are measuring. "copying" for a build, "uploading"
/// for a transfer, because the same event vocabulary serves both and "copying 40%"
/// during an upload.
///
/// The returned `Reporter` must be dropped before joining, or the channel never closes
/// and the join blocks forever.
fn printer(
    quiet: bool,
    verb: &'static str,
) -> (Reporter, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
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
                    // times over a 4 GiB transfer and a terminal is not a progress bar.
                    let percent = (fraction * 100.0) as u8;
                    if !quiet && percent != last_percent {
                        println!("  {verb} {detail}: {percent}%");
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
    (Reporter::new(tx), handle)
}

/// Upload an already-built image to a rack as a disk.
///
/// Everything is a flag, and nothing is prompted for: this has to work over SSH, in CI,
/// and from a script that has no terminal at all.
fn upload(args: &[String]) -> Result<()> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [image] = positional.as_slice() else {
        bail!("upload needs exactly one image path\n\n{USAGE}");
    };
    let opt = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };

    let image = PathBuf::from(image);
    if !image.is_file() {
        bail!("{} is not a file", image.display());
    }
    let project = opt("project").context("--project is required")?;
    let disk = opt("disk").context("--disk is required")?;

    // Only name a profile if one was actually asked for. Naming one unconditionally
    // stops the SDK consulting OXIDE_TOKEN, which is how this runs in CI.
    let selector = match opt("profile") {
        Some(name) => oxwin_rack::Selector::Profile(name),
        None => oxwin_rack::Selector::Environment,
    };

    let spec = oxwin_rack::DiskSpec {
        name: disk,
        description: opt("description")
            .unwrap_or_else(|| "Windows installer, built by oxwin".into()),
        block_size: opt("block-size")
            .as_deref()
            .unwrap_or("512")
            .parse()
            .context("--block-size must be 512, 2048 or 4096")?,
    };

    // Installed before the first request, so an interrupt during the upload is
    // caught rather than killing the process mid-import.
    let cancel = cancel_on_interrupt();
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(flag_in(args, "quiet"), "uploading");
    let result = rack.upload_image(&image, &spec, &reporter, &cancel);
    drop(reporter);
    printer.join().ok();

    let uploaded = result?;
    println!(
        "disk {} ready in project {project} ({:.2} GiB sent, {:.2} GiB skipped)",
        uploaded.disk,
        uploaded.sent as f64 / (1024.0 * 1024.0 * 1024.0),
        uploaded.skipped as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    Ok(())
}

/// Create the system disk and the instance, booting from an uploaded installer.
fn instance(args: &[String]) -> Result<()> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [name] = positional.as_slice() else {
        bail!("instance needs exactly one name\n\n{USAGE}");
    };
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };

    let project = opt("project").context("--project is required")?;
    let installer = opt("installer-disk")
        .context("--installer-disk is required: it becomes the boot disk")?;

    let mut spec = oxwin_rack::InstanceSpec::for_installer(name, &installer);
    if let Some(v) = opt("cpus") {
        spec.ncpus = v.parse().context("--cpus must be a number")?;
    }
    if let Some(v) = opt("memory-gib") {
        spec.memory_gib = v.parse().context("--memory-gib must be a number")?;
    }
    if let Some(v) = opt("system-disk") {
        spec.system_disk = Some(v);
    }
    if let Some(v) = opt("system-disk-gib") {
        spec.system_disk_gib =
            v.parse().context("--system-disk-gib must be a number")?;
    }
    spec.start = !flag_in(args, "no-start");

    let selector = match opt("profile") {
        Some(profile) => oxwin_rack::Selector::Profile(profile),
        None => oxwin_rack::Selector::Environment,
    };
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(flag_in(args, "quiet"), "creating");
    let result = rack.create_instance(&spec, &reporter);
    drop(reporter);
    printer.join().ok();

    let created = match result {
        Ok(created) => created,
        Err(failure) => {
            // Nothing is torn down. Say exactly what exists instead, so a retry does
            // not collide with it and the user is not left guessing.
            if !failure.leftovers.is_empty() {
                eprintln!("\nthese were created and have been left in place:");
                for disk in &failure.leftovers.disks {
                    eprintln!("  disk     {disk}");
                }
                for instance in &failure.leftovers.instances {
                    eprintln!("  instance {instance}");
                }
                eprintln!("\nto remove them:");
                for command in failure.leftovers.cleanup_commands(&project) {
                    eprintln!("  {command}");
                }
            }
            return Err(failure.error);
        }
    };

    println!(
        "instance {} created in project {project}, booting from {installer}",
        created.instance
    );
    if let Some(disk) = &created.system_disk {
        println!("  system disk: {disk}");
    }
    for warning in &created.warnings {
        println!("\nnote: {warning}");
    }
    println!(
        "\nwatch it:\n  oxide instance serial console --project {project} \
         --instance {}",
        created.instance
    );
    Ok(())
}

/// Wait for a golden install to finish and shut itself down.
///
/// Minutes to tens of minutes, depending on the release, and it prints a line a
/// minute so it is visibly alive: a UI
/// that does not move is a UI that has frozen as far as anyone watching it can
/// tell, and the natural response is to kill it.
fn watch(args: &[String]) -> Result<()> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [instance] = positional.as_slice() else {
        bail!("watch needs exactly one instance name\n\n{USAGE}");
    };
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let project = opt("project").context("--project is required")?;

    let mut opts = oxwin_rack::golden::watch::WatchOptions::default();
    if let Some(v) = opt("timeout") {
        opts.timeout = oxwin_rack::golden::watch::parse_duration(&v)?;
    }
    if let Some(v) = opt("poll") {
        opts.poll = oxwin_rack::golden::watch::parse_duration(&v)?;
    }

    let selector = match opt("profile") {
        Some(profile) => oxwin_rack::Selector::Profile(profile),
        None => oxwin_rack::Selector::Environment,
    };
    let cancel = cancel_on_interrupt();
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(flag_in(args, "quiet"), "waiting");
    let result = rack.watch_install(instance, &opts, &reporter, &cancel);
    drop(reporter);
    printer.join().ok();
    result?;
    println!("{instance} is generalized and stopped; it is ready to snapshot");
    Ok(())
}

/// The whole cycle: media in, a reusable image on the rack out.
///
/// Around twenty minutes against a nearby rack -- ten of them uploading, which is
/// the part that depends on where you are -- and resumable by re-running the
/// identical command.
fn golden(args: &[String]) -> Result<()> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [source] = positional.as_slice() else {
        bail!(
            "golden needs exactly one source: an ISO, a mount, or an .img\n\n\
             {USAGE}"
        );
    };
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let run =
        opt("run").context("--run is required: it names every resource")?;
    let names = oxwin_rack::Names::new(&run)?;
    let project = opt("project").context("--project is required")?;
    let quiet = flag_in(args, "quiet");

    let source = PathBuf::from(source);
    // An .img is already built. Anything else is media, and gets built first,
    // every time -- see build_for_golden for why a resume does not reuse the file
    // it finds.
    let is_image = source.extension().is_some_and(|e| e == "img");
    let (image_path, version) = if is_image {
        (
            source.clone(),
            opt("image-version").unwrap_or_else(|| "unknown".into()),
        )
    } else {
        let out = opt("image-out")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("{run}.img")));
        let detected = build_for_golden(&source, &out, args, quiet)?;
        (out, opt("image-version").unwrap_or(detected))
    };

    let mut spec = oxwin_rack::golden::GoldenSpec {
        names: names.clone(),
        image_path,
        keep: match opt("keep") {
            Some(v) => v.parse()?,
            None => oxwin_rack::Keep::default(),
        },
        watch: oxwin_rack::golden::WatchOptions::default(),
        system_disk_gib: 100,
        ncpus: 4,
        memory_gib: 8,
        os: opt("os").unwrap_or_else(|| "windows".into()),
        version,
    };
    if let Some(v) = opt("timeout") {
        spec.watch.timeout = oxwin_rack::golden::watch::parse_duration(&v)?;
    }
    if let Some(v) = opt("poll") {
        spec.watch.poll = oxwin_rack::golden::watch::parse_duration(&v)?;
    }
    if let Some(v) = opt("system-disk-gib") {
        spec.system_disk_gib =
            v.parse().context("--system-disk-gib must be a number")?;
    }
    if let Some(v) = opt("cpus") {
        spec.ncpus = v.parse().context("--cpus must be a number")?;
    }
    if let Some(v) = opt("memory-gib") {
        spec.memory_gib = v.parse().context("--memory-gib must be a number")?;
    }

    let selector = match opt("profile") {
        Some(profile) => oxwin_rack::Selector::Profile(profile),
        None => oxwin_rack::Selector::Environment,
    };
    let cancel = cancel_on_interrupt();
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(quiet, "working");
    let result = rack.run_golden(&spec, &reporter, &cancel);
    drop(reporter);
    printer.join().ok();

    match result {
        Ok(golden) => {
            println!("\nimage {} is ready in project {project}", golden.image);
            // Before the clone check, not after. The check takes several minutes,
            // and a report of what was left behind is no use to anyone once it is
            // sitting below an eight-minute wait.
            if !golden.leftovers.is_empty() {
                // Not a failure: the image exists. Say so, so nobody goes looking
                // for a problem with an image that is fine.
                println!(
                    "\nthe image is finished; these could not be tidied away:"
                );
                for resource in &golden.leftovers {
                    println!("  {}", resource.delete_command(&project));
                }
            }
            if flag_in(args, "verify-clone") {
                // After the image, never instead of it: a clone that fails to come
                // up is a fact about the image, and the image is still what the run
                // produced.
                verify_named(&names, &project, &selector, quiet, args)?;
            }
            Ok(())
        }
        Err(failure) => {
            // Nothing is torn down. Name what exists instead, then the one command
            // that carries on from here.
            if !failure.leftovers.is_empty() {
                eprintln!(
                    "\nthese exist on the rack and have been left in place:"
                );
                for instance in &failure.leftovers.instances {
                    eprintln!("  instance {instance}");
                }
                for disk in &failure.leftovers.disks {
                    eprintln!("  disk     {disk}");
                }
                for snapshot in &failure.leftovers.snapshots {
                    eprintln!("  snapshot {snapshot}");
                }
                for image in &failure.leftovers.images {
                    eprintln!("  image    {image}");
                }
            }
            eprintln!(
                "\n{}",
                oxwin_rack::golden::resume_hint(&names, &project)
            );
            if !failure.leftovers.is_empty() {
                eprintln!("\nor to start over, remove them:");
                for command in failure.leftovers.cleanup_commands(&project) {
                    eprintln!("  {command}");
                }
            }
            Err(failure.error)
        }
    }
}

/// Build media for a golden run, and report the release the media turned out to be.
///
/// `generalize` is set here, not taken from a flag. Media built without it installs
/// perfectly and never shuts down, and the watcher cannot tell that apart from a
/// hang -- it would spend its whole timeout on an install that worked.
///
/// **Rebuilds every time, including on a resume**, rather than reusing an image
/// file that happens to be at `out`.
///
/// The tempting alternative -- skip when the file exists -- cannot tell a finished
/// image from one an interrupted build left half-written, because both are just a
/// file of the right name. Uploading a truncated installer produces media that fails
/// somewhere inside Setup, long afterwards, for no visible reason. A rebuild costs a
/// few minutes against a cycle measured in the tens, so the cheap answer is also the
/// wrong one here.
fn build_for_golden(
    source: &std::path::Path,
    out: &std::path::Path,
    args: &[String],
    quiet: bool,
) -> Result<String> {
    let mut config = config_from_args(args)?;
    config.generalize = true;
    config.computer_name = "*".into();

    let media = if source.is_dir() {
        Media::Directory(source.to_path_buf())
    } else {
        Media::Iso(source.to_path_buf())
    };
    let request = Request {
        media,
        out: out.to_path_buf(),
        config,
        edition_hint: args
            .iter()
            .find_map(|a| a.strip_prefix("--edition-hint="))
            .map(str::to_string),
        ei_channel: args
            .iter()
            .find_map(|a| a.strip_prefix("--ei-channel="))
            .map(str::to_string),
        bare: false,
        assets: assets_from_args(args)?,
    };

    let (reporter, printer) = printer(quiet, "copying");
    let result = builder::build(&request, &reporter, &cancel_on_interrupt());
    drop(reporter);
    printer.join().ok();
    let output = result?;
    if !quiet {
        println!(
            "built {} ({}), {:.2} GiB copied",
            output.image_index,
            output.edition_id,
            output.copied_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        );
    }
    // The label, not the slug: this becomes the image's version string, which
    // someone reads in `oxide image list` months later.
    Ok(output.release.label().to_string())
}

/// Prove the image boots: clone it, and watch it come up and stay up.
///
/// Until this has been run, "golden image" is a claim rather than a feature.
fn verify(args: &[String]) -> Result<()> {
    let (names, project, selector, quiet) = golden_common(args, "verify")?;
    verify_named(&names, &project, &selector, quiet, args)
}

/// The clone check, given a run that has already been resolved.
///
/// Separate from [`verify`] because `golden --verify-clone` reaches it with the
/// *source path* as its positional argument, not a run name -- so re-parsing the
/// arguments there would try to make a resource name out of an ISO path and fail
/// immediately, at the very end of the run.
fn verify_named(
    names: &oxwin_rack::Names,
    project: &str,
    selector: &oxwin_rack::Selector,
    quiet: bool,
    args: &[String],
) -> Result<()> {
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let mut opts = oxwin_rack::golden::WatchOptions::default();
    if let Some(v) = opt("timeout") {
        opts.timeout = oxwin_rack::golden::watch::parse_duration(&v)?;
    }
    let cancel = cancel_on_interrupt();
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(quiet, "verifying");
    let result = rack.verify_clone(&names, &opts, &reporter, &cancel);
    drop(reporter);
    printer.join().ok();
    result?;

    println!(
        "\n{} came up and stayed up, so the image boots and does not \
         generalize itself.",
        names.clone_instance()
    );
    // The one thing a green result cannot stand in for. Said plainly rather than
    // implied, because it is the entire reason for generalizing.
    println!(
        "\nStill to check by hand, and it is the point of the exercise: log in \
         and confirm the clone's computer name and SID DIFFER from the machine \
         the image was taken from.\n  \
         hostname\n  \
         Get-CimInstance Win32_UserAccount | Select-Object SID"
    );
    println!(
        "\nwhen you are done with it:\n  \
         oxide instance delete --project {project} --instance {}\n  \
         oxide disk delete --project {project} --disk {}",
        names.clone_instance(),
        names.clone_disk()
    );
    Ok(())
}

/// Snapshot the system disk of a stopped, generalized instance.
fn snapshot(args: &[String]) -> Result<()> {
    let (names, project, selector, quiet) = golden_common(args, "snapshot")?;
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(quiet, "snapshotting");
    let result = rack.snapshot_step(&names, &reporter);
    drop(reporter);
    printer.join().ok();
    result?;
    println!("snapshot {} ready in project {project}", names.snapshot());
    Ok(())
}

/// Turn the snapshot into an image. The product of the cycle.
fn image(args: &[String]) -> Result<()> {
    let (names, project, selector, quiet) = golden_common(args, "image")?;
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(quiet, "imaging");
    let result = rack.image_step(
        &names,
        &opt("os").unwrap_or_else(|| "windows".into()),
        &opt("image-version").unwrap_or_else(|| "unknown".into()),
        &reporter,
    );
    drop(reporter);
    printer.join().ok();
    result?;
    println!("image {} ready in project {project}", names.image());
    Ok(())
}

/// Remove what a finished run no longer needs.
fn teardown(args: &[String]) -> Result<()> {
    let (names, project, selector, quiet) = golden_common(args, "teardown")?;
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let keep: oxwin_rack::Keep = match opt("keep") {
        Some(v) => v.parse()?,
        None => oxwin_rack::Keep::default(),
    };
    let rack = oxwin_rack::Rack::connect(&selector, &project)?;
    let (reporter, printer) = printer(quiet, "deleting");
    let result = rack.teardown_step(&names, keep, &reporter);
    drop(reporter);
    printer.join().ok();

    let stuck = result?;
    if stuck.is_empty() {
        // Only claim the image survives if it is actually there. `teardown` is
        // usable on a run that never got as far as making one, and saying "g5
        // remains" about an image that does not exist is a small lie that costs
        // someone a confused look at `oxide image list`.
        match rack.image_exists(&names.image()) {
            Ok(true) => println!("cleaned up; image {} remains", names.image()),
            _ => println!("cleaned up"),
        }
        return Ok(());
    }
    eprintln!("\nthese could not be removed and are still there:");
    for resource in &stuck {
        eprintln!("  {}", resource.delete_command(&project));
    }
    Ok(())
}

/// The four things every golden subcommand needs.
///
/// One run name in, every resource name derived from it — so these subcommands take
/// the same argument as `golden` and operate on the same run.
fn golden_common(
    args: &[String],
    command: &str,
) -> Result<(oxwin_rack::Names, String, oxwin_rack::Selector, bool)> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [run] = positional.as_slice() else {
        bail!("{command} needs exactly one run name\n\n{USAGE}");
    };
    let opt = |key: &str| -> Option<String> {
        let prefix = format!("--{key}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let project = opt("project").context("--project is required")?;
    let selector = match opt("profile") {
        Some(profile) => oxwin_rack::Selector::Profile(profile),
        None => oxwin_rack::Selector::Environment,
    };
    Ok((
        oxwin_rack::Names::new(run)?,
        project,
        selector,
        flag_in(args, "quiet"),
    ))
}

/// A [`Cancel`] wired to Ctrl-C.
///
/// Without this the CLI wont clean up after itself. A bulk import leaves the disk in a
/// state that **refuses deletion** until it is stopped, and `upload_image` runs that
/// teardown on cancellation, but only if it gets the chance. Killing the process instead
/// skips it entirely and strands the disk, it is what happened the first time this
///  was run against a rack.
///
/// The second Ctrl-C exits immediately. Teardown talks to the network, so it can hang,
/// and a cancel that cannot itself be cancelled  leaving the disk stranded.
fn cancel_on_interrupt() -> Cancel {
    let cancel = Cancel::new();
    let handler = cancel.clone();
    let result = ctrlc::set_handler(move || {
        if handler.is_cancelled() {
            eprintln!("\nabandoning. The disk may be left mid-import: run");
            eprintln!("  oxide disk import stop  --project <p> --disk <d>");
            eprintln!("  oxide disk import finalize --project <p> --disk <d>");
            eprintln!("before it can be deleted.");
            std::process::exit(130);
        }
        eprintln!("\ncancelling and cleaning up — Ctrl-C again to abandon");
        handler.cancel();
    });
    if result.is_err() {
        // Not fatal. A CLI that refuses to start because it could not install a signal
        // handler is worse than one that cannot be interrupted cleanly.
        eprintln!(
            "warn  could not install a Ctrl-C handler; interrupting may leave a disk mid-import"
        );
    }
    cancel
}

fn flag_in(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == &format!("--{name}"))
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

    // Which racks this machine is logged into. Not being logged in is a warning rather
    // than a failure: building and saving an image never touches a network.
    //
    // An expired token is called out because it is otherwise invisible until the first
    // request comes back 401, which reads as the rack being broken rather than as a
    // login having lapsed.
    match oxwin_rack::profiles() {
        Ok(profiles) if profiles.is_empty() => println!(
            "warn  no oxide login found — run `oxide auth login` to upload to a rack"
        ),
        Ok(profiles) => {
            for p in &profiles {
                let default = if p.is_default { " (default)" } else { "" };
                // An environment login records no account, so say the host alone
                // rather than printing "as None".
                let who = match &p.user {
                    Some(user) => format!(" as {user}"),
                    None => String::new(),
                };
                if p.is_expired_now() {
                    println!(
                        "warn  profile {}{default}: {}{who} — token expired, run \
                         `oxide auth login`",
                        p.name, p.host
                    );
                } else {
                    println!(
                        "ok    profile {}{default}: {}{who}",
                        p.name, p.host
                    );
                }
            }
        }
        Err(e) => {
            println!("FAIL  reading oxide credentials: {e}");
            failures += 1;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An explicit flag beats an inherited environment, the same rule `--assets`
    /// follows: what someone typed on this command line should win over whatever
    /// their shell happened to be carrying.
    #[test]
    fn an_explicit_flag_wins_over_a_file_and_the_environment() {
        let chosen = choose_password(
            Some("typed".into()),
            Some("/tmp/pw".into()),
            Some("inherited".into()),
        )
        .unwrap();
        assert_eq!(chosen, PasswordSource::Flag("typed".into()));
    }

    #[test]
    fn a_file_beats_the_environment() {
        let chosen =
            choose_password(None, Some("/tmp/pw".into()), Some("env".into()))
                .unwrap();
        assert_eq!(chosen, PasswordSource::File("/tmp/pw".into()));
    }

    #[test]
    fn the_environment_is_used_when_nothing_else_is_given() {
        let chosen = choose_password(None, None, Some("env".into())).unwrap();
        assert_eq!(chosen, PasswordSource::Environment("env".into()));
    }

    /// There is no default password anywhere in this workspace, and this is the
    /// place someone would be tempted to add one.
    #[test]
    fn nothing_given_is_an_error_not_a_default() {
        let error = choose_password(None, None, None).unwrap_err().to_string();
        assert!(error.contains("password is required"), "{error}");
        // And it must say how to supply one without exposing it in `ps`.
        assert!(error.contains("--password-file"), "{error}");
        assert!(error.contains("OXWIN_PASSWORD"), "{error}");
    }

    /// A password may legally begin or end with a space. Trimming one produces a
    /// machine nobody can log into, for a reason nobody can see.
    #[test]
    fn a_password_file_keeps_leading_and_trailing_spaces() {
        let dir = std::env::temp_dir().join(format!(
            "oxwin-pw-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pw");
        std::fs::write(&path, "  spaced secret  \nignored second line\n")
            .unwrap();
        let args = vec![format!("--password-file={}", path.display())];
        assert_eq!(password_from_args(&args).unwrap(), "  spaced secret  ");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A CRLF file, which is what someone editing on Windows will hand us.
    #[test]
    fn a_password_file_written_on_windows_works() {
        let dir = std::env::temp_dir().join(format!(
            "oxwin-pw-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pw");
        std::fs::write(&path, "secret\r\n").unwrap();
        let args = vec![format!("--password-file={}", path.display())];
        assert_eq!(password_from_args(&args).unwrap(), "secret");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_password_file_is_refused() {
        let dir = std::env::temp_dir().join(format!(
            "oxwin-pw-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pw");
        std::fs::write(&path, "\n").unwrap();
        let args = vec![format!("--password-file={}", path.display())];
        assert!(password_from_args(&args).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
