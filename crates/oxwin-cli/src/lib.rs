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
//! - `licenses` prints the third-party notices. The payload includes GPL'd EFI binaries,
//!   so an artifact has to be able to state its own terms without a network.
//!
//! This file is allowed to print. `oxwin-core` is not — see DEVELOPMENT.md.
//!
//! The entry point is [`run`], not a `main`: the shipped binary is `oxwin`, which
//! decides between this and the GUI from its arguments. See `crates/oxwin`.

use anyhow::{Context, Result, anyhow, bail};
use oxwin_core::builder::{self, Request};
use oxwin_core::engine::Cancel;
use oxwin_core::media::Media;
use oxwin_core::partition::{Format, Kind, Partition};
use oxwin_core::progress::{Event, Reporter};
use oxwin_core::settings::{CloudInit, Extra, WindowsRelease, extra_problems};
use oxwin_core::unattend::Config;
use std::path::PathBuf;

/// Every subcommand this CLI answers to, and the dispatcher's whole notion of what
/// counts as a command-line invocation rather than a click.
///
/// One definition, read by `oxwin::dispatch`, so a new subcommand cannot be added
/// here and silently routed to the GUI. `every_command_is_handled` holds the two
/// ends together.
pub const COMMANDS: &[&str] = &[
    "doctor", "build", "unattend", "upload", "instance", "watch", "snapshot",
    "image", "teardown", "golden", "verify", "licenses", "cidata",
];

/// Run the CLI. `args` is the argument list with the program name already removed.
///
/// An empty `args` means `doctor`, which is what it has always meant — the
/// dispatcher sends a bare invocation to the GUI, so this is reached only by
/// something that asked for the CLI with no command, such as `oxwin --`.
pub fn run(args: &[String]) -> Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("doctor");
    match cmd {
        "doctor" => doctor(),
        "build" => build(&args[1..]),
        "unattend" => unattend_cmd(&args[1..]),
        "upload" => upload(&args[1..]),
        "instance" => instance(&args[1..]),
        "watch" => watch(&args[1..]),
        "snapshot" => snapshot(&args[1..]),
        "image" => image(&args[1..]),
        "teardown" => teardown(&args[1..]),
        "golden" => golden(&args[1..]),
        "verify" => verify(&args[1..]),
        "licenses" => licenses(&args[1..]),
        "cidata" => cidata(&args[1..]),
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
       oxwin unattend [--sysprep] [--opt=value]
       oxwin upload <image.img> --project=<p> --disk=<name> [--opt=value]
       oxwin instance <name> --project=<p> --installer-disk=<d> [--opt=value]
       oxwin watch <instance> --project=<p> [--timeout=2h] [--poll=15s]
       oxwin snapshot <run> --project=<p>
       oxwin image <run> --project=<p> [--image-version=<v>]
       oxwin teardown <run> --project=<p> [--keep=image]
       oxwin golden <iso-or-mount-or-img> --run=<name> --project=<p>
       oxwin verify <run> --project=<p>
       oxwin licenses [--full]
       oxwin cidata <out.img> --hostname=<name> [--key=<k>] [--user-data=<file>]

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
  --verbose-serial       an OXIDE-STAGE marker on COM1 at the start of
                         specialize, so reaching it proves windowsPE finished
  --no-ems               do not patch the media BCD for serial; Windows Setup
                         then says nothing on COM1 until the install finishes
  --ui-language=<tag>    display language, e.g. de-DE. Needs a language pack on
                         the media; warns if the media does not carry it.
  --region=<tag>         formats and keyboard, e.g. de-DE. Works on any media.
  --timezone=<id>        Windows time zone ID, e.g. \"W. Europe Standard Time\".
                         Not an IANA name; default UTC.
  --log-path=<path>      where Setup writes setupact.log/setuperr.log. Its
                         default is the WinPE RAM disk, so a stalled install
                         loses its own explanation on reset
  --no-ui-on-error       WillShowUI=Never instead of OnError. On a guest with
                         no console, OnError is an infinite hang
  --ssh=0                do not install OpenSSH in the guest
  --rdp=0                do not enable RDP
  --windows=<ws2016|ws2019|ws2022|ws2025|win10|win11>
                         advisory: the media's own release overrides it
  --edition=<hint>       datacenter, standard, an index, or an EDITIONID;
                         omitted, the media's own list picks a sensible default
  --edition-hint=<hint>  overrides --edition when reading the WIM's image list
  --target-disk=<n>      disk index Setup installs to
  --partition=<spec>     repeatable; replaces the whole layout. Spec is
                         kind:size:letter:format:label, e.g.
                         --partition=efi:260::FAT32:System
                         --partition=msr:16
                         --partition=primary:extend:C:NTFS:Windows
                         Size may be \"extend\" for the rest of the disk. Omitted
                         entirely, the default EFI/MSR/Windows layout is used.
  --product-key=<key>
  --ei-channel=<Eval|_Default|none>
  --bare                 media only: no answer file, no drivers, no ei.cfg
  --assets=<dir>         a payload directory, instead of the embedded one
  --unattend=<file>      use this answer file instead of the generated one. It is
                         checked for known hazards and used regardless; only a file
                         that is not an answer file at all is refused.
  --no-cloud-init        do not install cloud-init. Without it a clone keeps
                         the image's hostname and its baked keys only
  --cloud-init-account=<keep|manage>
                         keep: the account and password you set here stay as
                         they are, and cloud-init only adds the instance's
                         keys (default). manage: cloud-init owns the account,
                         which replaces the password with a per-instance
                         random one the first time it runs on a clone
  --extra=<path>         repeatable; a file or directory copied onto the media
                         and into C:\\oxide\\extras in the guest. Nothing runs
                         it -- use cloud-init user-data for that
  --quiet

unattend options:
  --sysprep              print the generalize/oobeSystem answer file a golden
                         build would write, instead of the windowsPE/specialize
                         one a normal build writes
  plus every build option above that affects the answer file's content

upload options:
  --project=<name>       project to create the disk in. Required
  --disk=<name>          name for the new disk. Required
  --profile=<name>       which `oxide auth login` profile to use. Omitted, the
                         SDK resolves OXIDE_TOKEN, then OXIDE_PROFILE, then the
                         default profile — naming one here disables OXIDE_TOKEN
  --description=<text>   disk description. Omitted, it is just Installer for
                         Windows: an .img is all this path is given, so there
                         is no media here to read the release off
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
  --description=<text>   what `oxide instance list` shows. Omitted, it is just
                         Windows: this path is handed a disk, not media, so
                         there is nothing here to read the release off. A
                         `golden` run fills it in from the media it built
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
  --name=* or --generalize.

cidata options:
  A local NoCloud config drive, for exercising cloud-init under
  tools/qemu-test.sh --cloud-init before spending a rack cycle on it. The
  rack's own config drive comes from the control plane and is never built
  by this command.
  --hostname=<name>      local-hostname in meta-data. Required
  --instance-id=<id>     instance-id in meta-data. Default derived from
                         --hostname
  --key=<pubkey>         repeatable; an SSH public key line for public-keys
  --user-data=<file>     user_data, read verbatim from this file. Omitted,
                         user-data is written empty -- NoCloud wants it
                         present either way";

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
/// `extras` is the caller's own `extras_from_args(args)?`, taken as a
/// parameter rather than computed here so that a command that also needs the
/// `Vec<Extra>` itself -- `build`, `build_for_golden` -- walks the
/// filesystem once. Computing it twice was redundant I/O and a TOCTOU: a
/// directory `--extra` could change between the two walks, leaving
/// `has_extras` and the extras actually copied in disagreement.
fn config_from_args(args: &[String], extras: &[Extra]) -> Result<Config> {
    let opt = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let flag = |name: &str| args.iter().any(|a| a == &format!("--{name}"));

    // Iterated rather than matched, so a release added to `ALL` is accepted here without
    // anyone remembering to extend a literal list, which is how this arm came to accept
    // only ws2022 while the help text above advertised five tokens.
    //
    // The default is a starting point and almost always overwritten: `builder::assemble`
    // replaces it with the release read out of the media, and warns when the two disagree.
    let release = match opt("windows") {
        None => WindowsRelease::Server2022,
        Some(token) => *WindowsRelease::ALL
            .iter()
            .find(|r| r.token() == token)
            .ok_or_else(|| {
                let known: Vec<&str> =
                    WindowsRelease::ALL.iter().map(|r| r.token()).collect();
                anyhow!("unknown --windows={token}; known: {}", known.join(" "))
            })?,
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
        partitions: partitions_from_args(args)?,
        ui_language: opt("ui-language")
            .unwrap_or_else(|| oxwin_core::locale::DEFAULT_REGION.into()),
        region: opt("region")
            .unwrap_or_else(|| oxwin_core::locale::DEFAULT_REGION.into()),
        timezone: opt("timezone")
            .unwrap_or_else(|| oxwin_core::locale::DEFAULT_TIME_ZONE.into()),
        // An empty or whitespace-only `--product-key=` is not "no key was given" --
        // `opt` still returns `Some("")` -- and an empty `<Key>` element is not the
        // same as omitting it: Setup treats it as a key to resolve, matches no
        // edition, and stalls at the licence-terms page with no error. Trim first
        // and treat the empty result as `None`, matching what the GUI already does
        // in `Draft::to_settings`, which stores the trimmed value too.
        product_key: opt("product-key")
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty()),
        auto_logon: false,
        generalize: flag("generalize") || opt("name").as_deref() == Some("*"),
        verbose_serial: flag("verbose-serial"),
        log_path: opt("log-path"),
        show_ui_on_error: !flag("no-ui-on-error"),
        image_index: None,
        skip_image_install: opt("image-install").as_deref() == Some("0"),
        ssh_keys: opt("ssh-key")
            .unwrap_or_default()
            .split(';')
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect(),
        enable_ssh: opt("ssh").as_deref() != Some("0"),
        cloud_init: cloud_init_from_args(args)?,
        has_extras: !extras.is_empty(),
    };
    Ok(config)
}

/// `--no-cloud-init` and `--cloud-init-account=<keep|manage>`.
///
/// On by default, in the mode that leaves the typed account and password
/// alone: cloud-init only adds the instance's keys until something asks it to
/// do more. A typo in the mode is refused rather than silently landing on
/// `keep`, since that would leave the user believing cloud-init owns the
/// account when it does not; and naming a mode while also turning cloud-init
/// off is refused as a contradiction rather than guessed at.
fn cloud_init_from_args(args: &[String]) -> Result<Option<CloudInit>> {
    let off = flag_in(args, "no-cloud-init");
    let mode =
        args.iter().find_map(|a| a.strip_prefix("--cloud-init-account="));
    if off && mode.is_some() {
        bail!(
            "--no-cloud-init and --cloud-init-account= contradict each \
             other; use one or the other"
        );
    }
    if off {
        return Ok(None);
    }
    let manage_account = match mode {
        None => false,
        Some("keep") => false,
        Some("manage") => true,
        Some(other) => {
            bail!("unknown --cloud-init-account={other}; known: keep manage")
        }
    };
    Ok(Some(CloudInit { manage_account }))
}

/// `--extra=<path>`, repeatable. A file becomes `/extras/<file name>`; a
/// directory is walked and each file becomes
/// `/extras/<dir name>/<relative path>`, with `/` separators regardless of
/// host platform because this is a path on the Windows volume, not on the
/// machine doing the build.
///
/// `read_dir` order differs between filesystems, and that order decides which
/// clusters each file gets — see "The media file list must be sorted by the
/// path the file will have on the volume" in `CLAUDE.md` — so a directory is
/// walked with an explicit sort at every level rather than trusted as given.
/// A symlink inside a directory extra is followed: `fs::metadata` (which
/// follows links) rather than `symlink_metadata` decides file-vs-directory,
/// so the walk is deterministic in what it includes rather than in how it
/// arrived there.
fn extras_from_args(args: &[String]) -> Result<Vec<Extra>> {
    let mut extras = Vec::new();
    for path in args
        .iter()
        .filter_map(|a| a.strip_prefix("--extra="))
        .map(PathBuf::from)
    {
        let meta = std::fs::metadata(&path).with_context(|| {
            format!("--extra={}: not found", path.display())
        })?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                anyhow!(
                    "--extra={}: not a valid UTF-8 file name",
                    path.display()
                )
            })?
            .to_string();
        if meta.is_dir() {
            walk_extra_dir(&path, &format!("/extras/{name}"), &mut extras)?;
        } else {
            extras.push(Extra::from_file(path.clone())?);
        }
    }
    Ok(extras)
}

/// One level of `extras_from_args`' directory walk, called recursively.
///
/// `volume_prefix` already carries `/extras/<dir name>` (and, on recursion,
/// every subdirectory name below it), so each entry only has to add its own
/// name.
fn walk_extra_dir(
    dir: &std::path::Path,
    volume_prefix: &str,
    out: &mut Vec<Extra>,
) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("reading {}", dir.display()))?;
    entries.sort_unstable();

    for path in entries {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                anyhow!(
                    "--extra: {} is not a valid UTF-8 file name",
                    path.display()
                )
            })?
            .to_string();
        let volume_path = format!("{volume_prefix}/{name}");
        // Follows symlinks, so a link inside an --extra directory is walked
        // like the file or directory it points at, deterministically -- the
        // alternative, skipping links, is just as defensible but would leave
        // silently missing files with no message naming them.
        let meta = std::fs::metadata(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if meta.is_dir() {
            walk_extra_dir(&path, &volume_path, out)?;
        } else {
            out.push(Extra { source: path, volume_path });
        }
    }
    Ok(())
}

/// `--partition=<kind>:<size|extend>[:<letter>][:<format>][:<label>]`, repeated.
///
/// Positional rather than named because the alternative is five flags that have to
/// be kept in step across an unknown number of partitions. Absent entirely means the
/// default layout, which is the overwhelming case.
fn partitions_from_args(args: &[String]) -> Result<Vec<Partition>> {
    let specs: Vec<&str> =
        args.iter().filter_map(|a| a.strip_prefix("--partition=")).collect();
    if specs.is_empty() {
        return Ok(oxwin_core::partition::default_layout());
    }
    specs
        .iter()
        .map(|spec| {
            let f: Vec<&str> = spec.split(':').collect();
            let kind = match f.first().copied() {
                Some("efi") => Kind::Efi,
                Some("msr") => Kind::Msr,
                Some("primary") => Kind::Primary,
                other => bail!(
                    "unknown partition kind {other:?}; use efi, msr or primary"
                ),
            };
            let size_mb = match f.get(1).copied() {
                None | Some("extend") | Some("") => None,
                Some(mb) => Some(mb.parse().with_context(|| {
                    format!("partition size {mb:?} is not a number of MB")
                })?),
            };
            // Strict, like the kind and format fields beside it. Taking the
            // first character of whatever was typed accepted "C:" as C and a
            // stray space as nothing, and with the layout unvalidated until
            // recently nothing downstream looked at the result either.
            let letter = match f.get(2).copied() {
                None | Some("") => None,
                Some(l) => {
                    let mut chars = l.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) if c.is_ascii_alphabetic() => {
                            // Uppercased, because `os_partition_id` looks for
                            // C: a lowercase one pointed <InstallTo> at the
                            // wrong partition and said nothing.
                            Some(c.to_ascii_uppercase())
                        }
                        _ => bail!(
                            "drive letter {l:?} is not a single letter; write \
                             one letter, as in primary:extend:C:NTFS:Windows, \
                             or leave the field empty"
                        ),
                    }
                }
            };
            let format = match f.get(3).copied() {
                None | Some("") => None,
                Some("FAT32") | Some("fat32") => Some(Format::Fat32),
                Some("NTFS") | Some("ntfs") => Some(Format::Ntfs),
                Some(other) => {
                    bail!("unknown format {other:?}; use FAT32 or NTFS")
                }
            };
            let label =
                f.get(4).filter(|l| !l.is_empty()).map(|l| l.to_string());
            Ok(Partition { kind, size_mb, label, letter, format })
        })
        .collect()
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

    let extras = extras_from_args(args)?;
    // Refused here rather than left to the engine, so a collision between two
    // `--extra=` files is reported before an ISO gets opened and copied.
    if let Some(problem) =
        extra_problems(&extras).into_iter().find(|p| p.blocking)
    {
        bail!("{}", problem.message);
    }
    let config = config_from_args(args, &extras)?;
    let assets = assets_from_args(args)?;

    let unattend = match args.iter().find_map(|a| a.strip_prefix("--unattend="))
    {
        None => None,
        Some(path) => {
            let xml = std::fs::read_to_string(path)
                .with_context(|| format!("reading the answer file {path}"))?;
            // Advisory. Every rule here is a failure that costs a rack cycle to
            // discover, and none of them stops the build: the user edited this on
            // purpose and owns the outcome.
            let cx = oxwin_core::unattend::LintContext {
                target_disk: config.target_disk,
                generalize: config.generalize,
                cloud_init: config.cloud_init.is_some(),
            };
            for problem in oxwin_core::unattend::lint(&xml, &cx) {
                if problem.blocking {
                    bail!("{path}: {}", problem.message);
                }
                eprintln!("warn  {path}: {}", problem.message);
            }
            eprintln!(
                "note  using {path} verbatim. bootstrap.ps1 is still generated \
                 from the flags, but nothing else that shapes an answer file \
                 reaches this one: not the release detected from the media, \
                 not --ui-language, --region or --timezone, and not \
                 --partition."
            );
            Some(xml)
        }
    };

    let request = Request {
        media,
        out: PathBuf::from(out),
        config,
        edition_hint: opt("edition-hint"),
        ei_channel: opt("ei-channel"),
        bare: flag("bare"),
        assets,
        enable_ems: ems_enabled(args),
        unattend,
        extras,
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
        print_ems(&output.ems);
    }
    Ok(())
}

/// Print the answer file the current flags would produce.
///
/// Stdout, not a file beside the image: the answer file carries the password in
/// cleartext, and writing one out as a side effect of a build is how a secret ends
/// up somewhere nobody meant to put it.
fn unattend_cmd(args: &[String]) -> Result<()> {
    print!("{}", unattend_xml(args)?);
    Ok(())
}

/// What `unattend_cmd` prints, separated from the printing so a test can compare
/// it with `unattend::build` rather than with a captured stdout.
fn unattend_xml(args: &[String]) -> Result<String> {
    let extras = extras_from_args(args)?;
    let config = config_from_args(args, &extras)?;
    if flag_in(args, "sysprep") {
        oxwin_core::unattend::build_sysprep(&config)
    } else {
        oxwin_core::unattend::build(&config)
    }
}

/// Shared by `build` and `build_for_golden`, which otherwise printed the
/// identical match twice.
fn print_ems(ems: &builder::Ems) {
    match ems {
        builder::Ems::Patched { stores } => {
            println!("  ems:      COM1 @115200 ({stores} bcd stores)");
        }
        builder::Ems::Off(why) => {
            println!("  ems:      off ({why})");
        }
    }
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
/// and from a script that has no terminal at all. We can do things to optimize the upload
/// making this an actually useful feature.
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
        // An .img is all this path is given, so there is no media to read a release
        // off. `golden`, which built the image itself, fills it in.
        description: opt("description").unwrap_or_else(|| {
            oxwin_rack::Installed::default().installer_description()
        }),
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
    // Nothing here has seen the media -- the installer is a disk on the rack by now --
    // so the release cannot be detected, only stated.
    if let Some(v) = opt("description") {
        spec.description = v;
    }
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
/// minute so it is visibly alive: a UI that does not move is a UI that has
/// frozen as far as anyone watching it can tell, and the natural response is
/// to kill it.
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
    let (image_path, built) = if is_image {
        // An .img has no WIM to read, so the only thing known about it is whatever the
        // caller said. Passing that through as the release is better than describing
        // the instance as a bare "Windows" when they did name one.
        let version = opt("image-version");
        (
            source.clone(),
            BuiltMedia {
                installed: oxwin_rack::Installed {
                    release: version.clone(),
                    ..Default::default()
                },
                version: version.unwrap_or_else(|| "unknown".into()),
            },
        )
    } else {
        let out = opt("image-out")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("{run}.img")));
        let mut built = build_for_golden(&source, &out, args, quiet)?;
        // The override names the *image's* version. What the instance says it is
        // installing stays what the media said, because that is a fact.
        if let Some(v) = opt("image-version") {
            built.version = v;
        }
        (out, built)
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
        version: built.version,
        installed: built.installed,
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

/// What a golden build learned about its media, for the two resources that carry it.
struct BuiltMedia {
    /// The version string the finished image reports, read months later in
    /// `oxide image list`. The release label, not the slug.
    version: String,
    /// The same media, for the description of the instance that installs it.
    installed: oxwin_rack::Installed,
}

/// Build media for a golden run, and report what the media turned out to be.
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
) -> Result<BuiltMedia> {
    let extras = extras_from_args(args)?;
    if let Some(problem) =
        extra_problems(&extras).into_iter().find(|p| p.blocking)
    {
        bail!("{}", problem.message);
    }
    let mut config = config_from_args(args, &extras)?;
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
        enable_ems: ems_enabled(args),
        unattend: None,
        extras,
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
        print_ems(&output.ems);
    }
    // The label, not the slug: this becomes the image's version string, which
    // someone reads in `oxide image list` months later.
    //
    // Everything here comes off `output`, never off the flags: `assemble` overwrites
    // the asserted release with the detected one, so this is what was actually built.
    Ok(BuiltMedia {
        version: output.release.label().to_string(),
        installed: oxwin_rack::Installed {
            release: Some(output.release.label().to_string()),
            build: output.build,
            edition: Some(output.edition_id.clone()),
        },
    })
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
        // usable on a run that never got as far as making one, and saying "image
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

/// EMS is on by default: a guest with no framebuffer has no other way to show
/// Setup. `--no-ems` exists for the case where SAC framing on the serial
/// console gets in the way of something else.
fn ems_enabled(args: &[String]) -> bool {
    !args.iter().any(|a| a == "--no-ems")
}

/// Report whether this machine can build an image, and whether it can upload one.
///
/// Prints one line per check, prefixed `ok`, `warn` or `FAIL`, and exits 1 if anything
/// failed. The split between the last two is the whole point of the command: a **`FAIL`
/// means no image can be built from this binary at all** — the payload is missing or a
/// boot asset is truncated, neither of which is fixable by anything the user does at
/// run time — while a **`warn` means only the rack half is unavailable**, because
/// building and saving an image to a file never touches a network or the oxide CLI.
///
/// Everything here is deliberately passive: assets are read, `oxide` is looked up on
/// `PATH` rather than run, and credentials are inspected rather than exercised. Running
/// `doctor` cannot change anything or require auth, so it is safe to ask for first.
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
            // Cloud-init is on by default, so a payload without the MSI means
            // the default build cannot do what the tickbox says it does.
            match assets.read("cloudbase/CloudbaseInitSetup_x64.msi") {
                Ok(data) if !data.is_empty() => println!(
                    "ok    asset cloudbase-init MSI ({} bytes)",
                    data.len()
                ),
                _ => println!(
                    "warn  no cloudbase-init MSI in the payload — cloud-init \
                     builds will be refused; run ./tools/fetch-payload.sh"
                ),
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

/// The third-party notices, which a release has to convey rather than merely have.
///
/// Two of the EFI binaries the media carries are GPL'd, so this is an obligation and not
/// a courtesy — see `oxwin_core::notices` for why the terms live in the tree instead of
/// in the payload manifest. `--full` adds the license texts, which is what someone
/// auditing an artifact wants and what nobody wants in the middle of a build log.
fn licenses(args: &[String]) -> Result<()> {
    let full = args.iter().any(|a| a == "--full");
    if let Some(bad) = args.iter().find(|a| *a != "--full") {
        bail!("licenses takes only --full, not {bad:?}\n\n{USAGE}");
    }
    print!(
        "{}",
        if full {
            oxwin_core::notices::full()
        } else {
            oxwin_core::notices::summary()
        }
    );
    Ok(())
}

/// Write a local NoCloud config drive: `oxwin cidata`.
///
/// A test drive only, so this exists to be exercised under
/// `tools/qemu-test.sh --cloud-init` before a rack cycle is spent. Nothing
/// here prompts, and nothing here ships in a built image -- the rack's own
/// config drive comes from the control plane, through `oxwin_core::cidata`
/// carrying the same `Drive`/`build` this reaches through.
fn cidata(args: &[String]) -> Result<()> {
    let positional: Vec<&String> =
        args.iter().filter(|a| !a.starts_with("--")).collect();
    let [out] = positional.as_slice() else {
        bail!("cidata needs exactly one output path\n\n{USAGE}");
    };
    let opt = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter().find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    };
    let keys: Vec<String> = args
        .iter()
        .filter_map(|a| a.strip_prefix("--key="))
        .map(str::to_string)
        .collect();
    let hostname = opt("hostname").context("--hostname is required")?;
    let instance_id =
        opt("instance-id").unwrap_or_else(|| format!("i-{hostname}"));
    let user_data = match opt("user-data") {
        Some(path) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading --user-data={path}"))?,
        ),
        None => None,
    };

    let drive =
        oxwin_core::cidata::Drive { hostname, instance_id, keys, user_data };
    let bytes = oxwin_core::cidata::build(&drive)?;
    let out = PathBuf::from(out);
    std::fs::write(&out, &bytes)
        .with_context(|| format!("writing {}", out.display()))?;
    println!(
        "wrote {} ({:.1} MiB, label {})",
        out.display(),
        bytes.len() as f64 / (1024.0 * 1024.0),
        oxwin_core::cidata::LABEL
    );
    Ok(())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|c| c.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `COMMANDS` is what the dispatcher in `crates/oxwin` treats as "this is a
    /// command line, not a click", and `run`'s `match` is what actually happens. A
    /// subcommand added to one and not the other fails in the worst possible way:
    /// `oxwin newthing` opens a window instead of reporting anything.
    ///
    /// Nothing can inspect a `match` at runtime, so this reads the arms out of this
    /// file's own source. Ugly, and the alternative is a pair of lists nobody
    /// diffs. `-h`/`--help`/`help` are excluded deliberately: they are handled here
    /// but the dispatcher recognises them separately, since a bare `--help` has to
    /// reach the CLI without being a subcommand.
    #[test]
    fn every_command_is_handled() {
        // `include_str!` embeds this file's bytes verbatim, so on a checkout with
        // `core.autocrlf=true` — the default on GitHub's windows runners, and what
        // `.gitattributes` exempts only the goldens from — every line ends `\r\n`
        // and a `"\n}\n"` sentinel matches nothing. The arm parsing below survives
        // that because it trims each line; this did not, and the failure was
        // `run() ends` on windows only, naming nothing to do with the match the
        // test exists to check.
        let source = include_str!("lib.rs").replace("\r\n", "\n");
        let body = source
            .split_once("pub fn run(args: &[String]) -> Result<()> {")
            .expect("run() is where the dispatch match lives")
            .1
            .split_once("\n}\n")
            .expect("run() ends")
            .0;

        let mut arms: Vec<&str> = body
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let name = line.strip_prefix('"')?;
                let (name, rest) = name.split_once('"')?;
                rest.trim_start().starts_with("=>").then_some(name)
            })
            .filter(|name| !matches!(*name, "-h" | "--help" | "help"))
            .collect();
        arms.sort_unstable();

        let mut expected = COMMANDS.to_vec();
        expected.sort_unstable();

        assert_eq!(
            arms, expected,
            "COMMANDS and run()'s match disagree; a command in one and not the \
             other is a command the dispatcher sends to the wrong front end"
        );
    }

    /// Someone reading `--help` should see every command they can type.
    #[test]
    fn usage_names_every_command() {
        for cmd in COMMANDS {
            assert!(
                USAGE.contains(&format!("oxwin {cmd}")),
                "USAGE does not mention {cmd}"
            );
        }
    }

    /// On by default, in the mode that does not touch the password.
    #[test]
    fn cloud_init_defaults_to_on_and_keeps_the_account() {
        let c = cloud_init_from_args(&args(&[])).unwrap();
        assert_eq!(c, Some(CloudInit { manage_account: false }));
    }

    #[test]
    fn no_cloud_init_turns_it_off() {
        assert_eq!(
            cloud_init_from_args(&args(&["--no-cloud-init"])).unwrap(),
            None
        );
    }

    #[test]
    fn the_account_mode_is_explicit() {
        assert_eq!(
            cloud_init_from_args(&args(&["--cloud-init-account=manage"]))
                .unwrap(),
            Some(CloudInit { manage_account: true })
        );
        assert_eq!(
            cloud_init_from_args(&args(&["--cloud-init-account=keep"]))
                .unwrap(),
            Some(CloudInit { manage_account: false })
        );
    }

    /// A typo must not silently pick a mode. Choosing `keep` for
    /// `--cloud-init-account=mange` would leave the user believing cloud-init
    /// owns the account when it does not.
    #[test]
    fn an_unknown_account_mode_is_refused() {
        let err = cloud_init_from_args(&args(&["--cloud-init-account=mange"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("keep"), "{err}");
        assert!(err.contains("manage"), "{err}");
    }

    /// Asking for a mode and also turning cloud-init off is a contradiction,
    /// and guessing which one was meant is how a user ends up with an image
    /// they did not ask for.
    #[test]
    fn a_mode_with_cloud_init_off_is_refused() {
        assert!(
            cloud_init_from_args(&args(&[
                "--no-cloud-init",
                "--cloud-init-account=manage"
            ]))
            .is_err()
        );
    }

    /// Repeatable, and the volume path is built from the file's name -- never
    /// from anything the user types, which is what keeps it inside /extras.
    #[test]
    fn extras_are_repeatable_and_land_under_extras() {
        let dir = std::env::temp_dir()
            .join(format!("oxwin-extras-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.zip"), b"a").unwrap();
        std::fs::write(dir.join("b.txt"), b"b").unwrap();
        let extras = extras_from_args(&args(&[
            &format!("--extra={}", dir.join("a.zip").display()),
            &format!("--extra={}", dir.join("b.txt").display()),
        ]))
        .unwrap();
        let paths: Vec<&str> =
            extras.iter().map(|e| e.volume_path.as_str()).collect();
        assert_eq!(paths, ["/extras/a.zip", "/extras/b.txt"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A directory keeps its structure, so `--extra=./tools` is one flag rather
    /// than one per file.
    #[test]
    fn a_directory_extra_preserves_its_structure() {
        let dir = std::env::temp_dir()
            .join(format!("oxwin-extras-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/c.txt"), b"c").unwrap();
        let extras =
            extras_from_args(&args(&[&format!("--extra={}", dir.display())]))
                .unwrap();
        assert_eq!(extras.len(), 1);
        assert!(
            extras[0].volume_path.ends_with("/inner/c.txt"),
            "{}",
            extras[0].volume_path
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `has_extras` is derived from the same list a caller walked, not from a
    /// second walk of the filesystem -- passing an empty slice must turn it
    /// off even with `--extra=` on the command line, and passing the list
    /// `extras_from_args` actually returned must turn it on. Two walks that
    /// could disagree (a directory changing between them) is the bug this
    /// guards against.
    #[test]
    fn has_extras_comes_from_the_extras_passed_in_not_a_second_walk() {
        let dir = std::env::temp_dir()
            .join(format!("oxwin-extras-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.zip"), b"a").unwrap();
        let flags =
            args(&[&format!("--extra={}", dir.join("a.zip").display())]);
        let password = "--password=0xide!230xide!23".to_string();
        let mut with_password = flags.clone();
        with_password.push(password);

        let extras = extras_from_args(&flags).unwrap();
        assert_eq!(extras.len(), 1);

        let config = config_from_args(&with_password, &extras).unwrap();
        assert!(config.has_extras);

        let config_without = config_from_args(&with_password, &[]).unwrap();
        assert!(!config_without.has_extras);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_extra_is_refused_by_the_flag() {
        let err = extras_from_args(&args(&["--extra=/nonexistent/x.zip"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("x.zip"), "{err}");
    }

    #[test]
    fn ems_is_on_unless_refused() {
        assert!(ems_enabled(&[]));
        assert!(ems_enabled(&["--verbose-serial".into()]));
        assert!(!ems_enabled(&["--no-ems".into()]));
    }

    /// `--product-key=` with nothing after the `=` is not "install with no key" by
    /// accident -- it must become `None`, not `Some("")`. An empty `<Key>` element
    /// in the answer file is not the same as omitting it: Setup treats it as a key
    /// to resolve, matches no edition, and stalls at the licence-terms page with no
    /// error anywhere.
    #[test]
    fn an_empty_product_key_flag_is_none() {
        let args = vec![
            "--password=0xide!230xide!23".to_string(),
            "--product-key=".to_string(),
        ];
        let config = config_from_args(&args, &[]).unwrap();
        assert_eq!(config.product_key, None);
    }

    /// Whitespace-only is the same defect wearing a disguise.
    #[test]
    fn a_whitespace_only_product_key_flag_is_none() {
        let args = vec![
            "--password=0xide!230xide!23".to_string(),
            "--product-key=   ".to_string(),
        ];
        let config = config_from_args(&args, &[]).unwrap();
        assert_eq!(config.product_key, None);
    }

    /// The CLI stores the trimmed key, exactly what the GUI's `Draft::to_settings`
    /// does -- not the untrimmed original with a comment merely claiming parity.
    #[test]
    fn a_product_key_with_surrounding_whitespace_is_trimmed() {
        let args = vec![
            "--password=0xide!230xide!23".to_string(),
            "--product-key= WX4NM-KYWYW-QJJR4-XV3QB-6VM33 ".to_string(),
        ];
        let config = config_from_args(&args, &[]).unwrap();
        assert_eq!(
            config.product_key,
            Some("WX4NM-KYWYW-QJJR4-XV3QB-6VM33".to_string())
        );
    }

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

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    /// The documented spec forms, each one a row. `--partition` is the only
    /// place a layout can be typed on the CLI, and until the builder started
    /// checking the result nothing downstream read it at all.
    #[test]
    fn partition_specs_parse_as_documented() {
        let cases: &[(&str, Partition)] = &[
            (
                "efi:260::FAT32:System",
                Partition {
                    kind: Kind::Efi,
                    size_mb: Some(260),
                    label: Some("System".into()),
                    letter: None,
                    format: Some(Format::Fat32),
                },
            ),
            // Short: everything after the size omitted entirely.
            (
                "msr:16",
                Partition {
                    kind: Kind::Msr,
                    size_mb: Some(16),
                    label: None,
                    letter: None,
                    format: None,
                },
            ),
            // `extend` is the rest of the disk, which is `None` in the model.
            (
                "primary:extend:C:NTFS:Windows",
                Partition {
                    kind: Kind::Primary,
                    size_mb: None,
                    label: Some("Windows".into()),
                    letter: Some('C'),
                    format: Some(Format::Ntfs),
                },
            ),
            // An empty size field means the same as `extend`.
            (
                "primary::D:ntfs:Data",
                Partition {
                    kind: Kind::Primary,
                    size_mb: None,
                    label: Some("Data".into()),
                    letter: Some('D'),
                    format: Some(Format::Ntfs),
                },
            ),
            // A lowercase letter is uppercased: `os_partition_id` looks for C,
            // so a lowercase one would point <InstallTo> elsewhere in silence.
            (
                "primary:1024:e:fat32:",
                Partition {
                    kind: Kind::Primary,
                    size_mb: Some(1024),
                    label: None,
                    letter: Some('E'),
                    format: Some(Format::Fat32),
                },
            ),
        ];
        for (spec, want) in cases {
            let got =
                partitions_from_args(&args(&[&format!("--partition={spec}")]))
                    .unwrap_or_else(|e| panic!("{spec:?}: {e}"));
            assert_eq!(got, vec![want.clone()], "{spec:?}");
        }
    }

    /// No `--partition` at all is the overwhelming case, and it has to be the
    /// layout every committed golden was built with.
    #[test]
    fn no_partition_flag_is_the_default_layout() {
        assert_eq!(
            partitions_from_args(&args(&["--password=x"])).unwrap(),
            oxwin_core::partition::default_layout()
        );
    }

    /// Repeated flags replace the whole layout, in the order given.
    #[test]
    fn repeated_partition_flags_are_the_whole_layout() {
        let got = partitions_from_args(&args(&[
            "--partition=efi:260::FAT32:System",
            "--partition=primary:extend:C:NTFS:Windows",
        ]))
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].kind, Kind::Efi);
        assert_eq!(got[1].kind, Kind::Primary);
    }

    /// Every malformed field is refused rather than silently reinterpreted. The
    /// drive letter is the one that was not: it took the first character of
    /// whatever it was given, so "C:" was C and " " was nothing.
    #[test]
    fn malformed_partition_specs_are_refused() {
        let cases: &[(&str, &str)] = &[
            ("disk:100", "unknown partition kind"),
            ("primary:100:C:EXFAT:Data", "unknown format"),
            ("primary:sixty:C:NTFS:Data", "not a number of MB"),
            ("primary:100:CD:NTFS:Data", "not a single letter"),
            ("primary:100: :NTFS:Data", "not a single letter"),
            ("primary:100:3:NTFS:Data", "not a single letter"),
        ];
        for (spec, expected) in cases {
            let err =
                partitions_from_args(&args(&[&format!("--partition={spec}")]))
                    .err()
                    .unwrap_or_else(|| panic!("{spec:?} was accepted"));
            let text = format!("{err:#}");
            assert!(
                text.contains(expected),
                "{spec:?}: wanted {expected:?}, got {text:?}"
            );
        }
    }

    /// `oxwin unattend` exists to show what a build would write. If it can
    /// diverge from what a build actually writes it is worse than nothing.
    #[test]
    fn unattend_prints_what_a_build_would_write() {
        // A password file rather than `--password=`, which prints a warning:
        // a test that scrolls a warning past every run teaches people to
        // ignore the real one.
        let dir = std::env::temp_dir().join(format!(
            "oxwin-pw-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pw = dir.join("pw");
        std::fs::write(&pw, "0xide!230xide!23\n").unwrap();
        let flags = args(&[
            &format!("--password-file={}", pw.display()),
            "--name=win-server-01",
            "--edition=datacenter",
            "--region=de-DE",
            "--timezone=W. Europe Standard Time",
            "--ui-language=de-DE",
            "--partition=efi:260::FAT32:System",
            "--partition=msr:16",
            "--partition=primary:extend:C:NTFS:Windows",
        ]);
        let config = config_from_args(&flags, &[]).unwrap();
        assert_eq!(
            unattend_xml(&flags).unwrap(),
            oxwin_core::unattend::build(&config).unwrap()
        );

        // And `--sysprep` prints the other document, not the same one.
        let mut sysprep = flags.clone();
        sysprep.push("--sysprep".into());
        let config = config_from_args(&sysprep, &[]).unwrap();
        assert_eq!(
            unattend_xml(&sysprep).unwrap(),
            oxwin_core::unattend::build_sysprep(&config).unwrap()
        );
        assert_ne!(
            unattend_xml(&sysprep).unwrap(),
            unattend_xml(&flags).unwrap()
        );
    }
}
