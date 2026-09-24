// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Cloud-init (cloudbase-init) in the guest.
//!
//! Upstream cloudbase-init, not the 2022 Oxide fork the public docs still point
//! at: both of that fork's functional patches are in upstream master, and
//! upstream is versioned, pinnable and EV code-signed. See
//! `docs/superpowers/specs/2026-09-22-cloud-init-design.md`.
//!
//! Two conf files, because there are two runs with different jobs:
//!
//! - `cloudbase-init-unattend.conf` is the one-shot run in the `specialize`
//!   pass of a clone's first boot: hostname, volume extension, NTP, RDP. It
//!   must not touch the account, because the account has no profile yet.
//! - `cloudbase-init.conf` is the service, every boot: SSH keys from the
//!   metadata drive, `user_data`, and -- in Mode B only -- the account itself.
//!
//! Both are pinned byte for byte by goldens in `testdata/cloudbase/`.

use crate::bootstrap::GENERALIZED_MARKER;
use crate::unattend::Config;

/// Where the MSI lands on the installer volume.
pub const MSI_VOLUME_PATH: &str = "/cloudbase/CloudbaseInitSetup_x64.msi";
/// The same file as the guest names it, under the media root `$root`. Two
/// spellings of one fact, held together by a test: `VOLUME_LABEL` once drifted
/// between two places and the bootstrap was skipped on every install while
/// installs still appeared to succeed.
pub const MSI_MEDIA_SUBPATH: &str = r"cloudbase\CloudbaseInitSetup_x64.msi";
pub const SERVICE_CONF_VOLUME_PATH: &str = "/cloudbase/cloudbase-init.conf";
pub const UNATTEND_CONF_VOLUME_PATH: &str =
    "/cloudbase/cloudbase-init-unattend.conf";

/// Everything this tool leaves in a guest: the logs, the clone-side runner and
/// the extras. Its ACL is set by [`install_block`], because it holds a script
/// SYSTEM runs.
pub const OXIDE_DIR: &str = r"C:\oxide";

/// Where both runs log. Created by the bootstrap, because nothing establishes
/// that cloudbase-init creates a missing `log_dir` itself.
pub const LOG_DIR: &str = r"C:\oxide\log";

/// The unattend run's log, under [`LOG_DIR`]. Named once: the conf sets it and
/// the clone runner reads it back to tell a run that found no config drive
/// from one that worked -- cloudbase-init exits 0 either way.
const UNATTEND_LOG_FILE: &str = "cloudbase-init-unattend.log";

/// Where the MSI puts mtools (`mlabel.exe`, `mcopy.exe`, `mdir.exe`), which
/// cloudbase-init needs to read a vFAT config drive at all:
/// `cloudbaseinit/utils/windows/vfat.py` (1.1.8) raises `"mtools_path" needs
/// to be provided in order to access VFAT drives` when it is unset, the
/// config-drive search swallows that as a warning, and the run ends `No
/// metadata service found` -- no hostname, no keys, no user_data, exit 0. The
/// installer writes this option into its *own* `cloudbase-init.conf`, which
/// ours replaces, so ours has to carry it. Confirmed on a guest: all three
/// tools are in this directory after a 64-bit install. The bootstrap warns if
/// the install landed anywhere else.
const MTOOLS_PATH: &str =
    r"C:\Program Files\Cloudbase Solutions\Cloudbase-Init\bin\";

/// Where the clone-side runner is written in the guest. Short on purpose: it is
/// named by a `RunSynchronousCommand`, and `<Path>` is capped at 259
/// characters -- the prototype's inline cmd.exe incantation was ~230 and one
/// edit away from Setup rejecting the whole answer file.
pub const CLONE_SCRIPT_PATH: &str = r"C:\oxide\cloud-init.ps1";

/// The startup task that brings the service up, and in Mode A creates the
/// profile first.
pub const TASK_NAME: &str = "OxideCloudInit";

/// A volume path as the guest names it under `$root`.
fn media_subpath(volume_path: &str) -> String {
    volume_path.trim_start_matches('/').replace('/', "\\")
}

/// The last component of a volume path: the name a conf file is installed
/// under, which is the name the service and the runner look for.
fn file_name(volume_path: &str) -> &str {
    volume_path.rsplit('/').next().unwrap_or(volume_path)
}

/// A PowerShell single-quoted string literal. PowerShell also reads the four
/// typographic single quotes as quote characters, so they are doubled too --
/// the same escaping `bootstrap` gives SSH keys, made complete.
fn ps_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
        {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The one metadata service. The stock list probes HTTP endpoints that do not
/// exist on a rack, and a guest with no route to them spends its first boot
/// timing out -- a trap in CLAUDE.md.
const METADATA_SERVICE: &str =
    "cloudbaseinit.metadata.services.nocloudservice.NoCloudConfigDriveService";

/// A golden image keeps `<ComputerName>*</ComputerName>` *and* runs
/// `SetHostNamePlugin`, deliberately: `*` guarantees a valid unique name if the
/// config drive is ever missing, and the plugin overwrites it with the
/// instance's name when it is there. A named machine gets neither -- the user
/// typed that name.
fn is_golden(config: &Config) -> bool {
    config.computer_name == "*"
}

fn manages_account(config: &Config) -> bool {
    config.cloud_init.is_some_and(|c| c.manage_account)
}

/// Join the lines the way Windows will read them.
fn crlf(lines: Vec<String>) -> String {
    lines.join("\r\n") + "\r\n"
}

/// The `[config_drive]` block.
///
/// Read from cloudbase-init 1.1.8 (tag exists; commit
/// `a22ac6c4c0850759ae1e7acb2f5d26c2f5afd4d7`), `cloudbaseinit/conf/cloudconfig.py`
/// (despite the filename, it registers the `[config_drive]` option group) and
/// `cloudbaseinit/metadata/services/baseconfigdrive.py`. `types` and
/// `locations` are the current option names; `raw_hdd`, `cdrom` and `vfat` are
/// deprecated booleans kept for backward compatibility that only *add* to
/// whatever `types`/`locations` already contain -- they never narrow the
/// search. `types` and `locations` both default to every accepted value
/// (`{vfat, iso}` / `{cdrom, hdd, partition}`), so setting them alone would not
/// exclude `iso`/`cdrom`: `raw_hdd` and `cdrom` default to `true` and would add
/// `iso`/`cdrom` straight back in. An Oxide config drive is a vFAT volume on a
/// whole disk, so this pins `types=vfat`, `locations=hdd`, and turns the two
/// widening flags off; `vfat=true` (its default) only adds `vfat`/`hdd`, which
/// is already the configured set, so it is left unset.
///
/// The two flags make cloudbase-init log `Deprecated: Option "raw_hdd" from
/// group "config_drive" is deprecated for removal` (and the same for `cdrom`)
/// on every run -- seen on the first QEMU clone. That warning is accepted, not
/// fixed: it is the only way to narrow the search in 1.1.8. Dropping either
/// flag lets it default to `true`, which adds `iso` to the types, and the
/// search is the product of types and locations, so `hdd` x `iso` comes back
/// with it. That probe (`_get_config_drive_from_raw_hdd` ->
/// `_extract_iso_from_devices` in `osconfigdrive/windows.py`) checks no label:
/// it copies the first fixed disk carrying an ISO9660 signature to a temp file
/// and, if `bsdtar` extracts it, takes it as the config drive -- a Windows ISO
/// imported as a disk and left attached would do. (`bsdtar_path` is unset
/// here, so today that copy would end in a warning; nothing should rest on an
/// option nobody chose to leave out.) The product iterates two Python sets, so whether
/// that probe runs before `hdd` x `vfat` is down to hash seeding.
fn config_drive_block() -> Vec<String> {
    vec![
        "[config_drive]".into(),
        "types=vfat".into(),
        "locations=hdd".into(),
        "raw_hdd=false".into(),
        "cdrom=false".into(),
    ]
}

/// The one-shot `specialize` run on each clone's first boot.
pub fn unattend_conf(config: &Config) -> String {
    // Plugin paths verified against the 1.1.8 tree (each file read, its
    // `class` line grepped). NOTE: NTPClientPlugin lives under `.windows.`,
    // not `.common.` -- `cloudbaseinit/conf/default.py`'s stock `plugins`
    // list registers `cloudbaseinit.plugins.windows.ntpclient.NTPClientPlugin`,
    // and `cloudbaseinit/plugins/common/ntpclient.py`'s `NTPClientPlugin` is
    // the base class the Windows one subclasses, not something the plugin
    // factory is ever pointed at directly.
    let mut plugins: Vec<&str> =
        vec!["cloudbaseinit.plugins.windows.ntpclient.NTPClientPlugin"];
    if is_golden(config) {
        plugins
            .push("cloudbaseinit.plugins.common.sethostname.SetHostNamePlugin");
    }
    plugins.push(
        "cloudbaseinit.plugins.windows.extendvolumes.ExtendVolumesPlugin",
    );
    if config.enable_rdp {
        plugins.push("cloudbaseinit.plugins.windows.rdp.RDPSettingsPlugin");
    }

    let mut lines = vec![
        "# Generated by the Oxide Windows image builder. The one-shot run in"
            .into(),
        "# the specialize pass of a clone's first boot.".into(),
        "[DEFAULT]".into(),
        // The specialize pass owns the reboot: WillReboot=OnRequest reads exit
        // code 1 as "reboot requested", so cloud-init must not reboot itself.
        "allow_reboot=false".into(),
        "stop_service_on_exit=false".into(),
        // No route off the rack, and asking costs a timeout.
        "check_latest_version=false".into(),
        "netbios_host_name_compatibility=false".into(),
        "ntp_enable_service=true".into(),
        "real_time_clock_utc=true".into(),
        // The installer's own conf carries this; ours replaces that file.
        format!("mtools_path={MTOOLS_PATH}"),
    ];
    if config.enable_rdp {
        lines.push("rdp_set_keepalive=true".into());
    }
    lines.extend([
        "verbose=true".into(),
        "debug=true".into(),
        format!("log_dir={LOG_DIR}"),
        format!("log_file={UNATTEND_LOG_FILE}"),
        format!("metadata_services={METADATA_SERVICE}"),
        format!("plugins={}", plugins.join(",")),
    ]);
    lines.extend(config_drive_block());
    crlf(lines)
}

/// The service, every boot.
pub fn service_conf(config: &Config) -> String {
    let mut plugins: Vec<&str> = Vec::new();
    if manages_account(config) {
        // Mode B. Creates the Windows profile the key plugin needs -- and
        // resets the password doing it, because upstream's
        // BaseCreateUserPlugin::execute calls post_create_user on every path.
        plugins
            .push("cloudbaseinit.plugins.windows.createuser.CreateUserPlugin");
    }
    if is_golden(config) {
        plugins
            .push("cloudbaseinit.plugins.common.sethostname.SetHostNamePlugin");
    }
    plugins.extend([
        "cloudbaseinit.plugins.common.sshpublickeys.SetUserSSHPublicKeysPlugin",
        "cloudbaseinit.plugins.windows.extendvolumes.ExtendVolumesPlugin",
        "cloudbaseinit.plugins.common.userdata.UserDataPlugin",
    ]);

    let mut lines = vec![
        "# Generated by the Oxide Windows image builder. The cloudbase-init"
            .into(),
        "# service, which runs on every boot.".into(),
        "[DEFAULT]".into(),
        format!("username={}", config.username),
        "groups=Administrators".into(),
        "inject_user_password=false".into(),
        "allow_reboot=false".into(),
        "stop_service_on_exit=false".into(),
        "check_latest_version=false".into(),
        "netbios_host_name_compatibility=false".into(),
        "real_time_clock_utc=true".into(),
        // The installer's own conf carries this; ours replaces that file.
        format!("mtools_path={MTOOLS_PATH}"),
    ];
    if config.enable_rdp {
        lines.push("rdp_set_keepalive=true".into());
    }
    lines.extend([
        "verbose=true".into(),
        "debug=true".into(),
        format!("log_dir={LOG_DIR}"),
        "log_file=cloudbase-init.log".into(),
        format!("metadata_services={METADATA_SERVICE}"),
        format!("plugins={}", plugins.join(",")),
        // Both shapes of user_data: #cloud-config YAML and a
        // <powershell>/<script> blob.
        "user_data_plugins=cloudbaseinit.plugins.common.userdataplugins.cloudconfig.CloudConfigPlugin,cloudbaseinit.plugins.common.userdataplugins.shellscript.ShellScriptPlugin".into(),
    ]);
    lines.extend(config_drive_block());
    crlf(lines)
}

// The PowerShell fragments below are spliced into `bootstrap.ps1`, which runs
// as SYSTEM in the `specialize` pass under `$ErrorActionPreference = 'Stop'`.
// Every fragment is LF-separated (`bootstrap` joins with CRLF), ASCII only
// (the guest writes scripts with `-Encoding ASCII`), and wrapped so that a
// throw is logged rather than aborting the drivers, the ESP fallback and
// everything else after it.
//
// Two scripts are written *from* `bootstrap.ps1` as single-quoted
// here-strings, so nothing in them is expanded until they run on a later boot.
// Anything that has to be fixed at build time -- the account name, whether
// this is a golden image -- is therefore spliced in here, by Rust, as literal
// text. Never the password: see `profile_step`. Those do not nest, and a line starting `'@` inside
// one ends it: nothing below may start a line that way, which
// `the_here_strings_balance` checks.

/// The `specialize`-time install: verify and install the MSI, copy the confs,
/// disable the service, write the clone-side runner (golden images only) and
/// register `OxideCloudInit`. Empty when cloud-init is off.
pub fn install_block(config: &Config) -> String {
    if config.cloud_init.is_none() {
        return String::new();
    }
    let golden = config.generalize;
    let service_conf = media_subpath(SERVICE_CONF_VOLUME_PATH);
    let unattend_conf = media_subpath(UNATTEND_CONF_VOLUME_PATH);

    let mut s = String::new();
    s.push_str(&oxide_dir_acl());
    s.push('\n');
    s.push_str(
        r#"# Cloud-init (cloudbase-init). Verified on the machine itself before install:
# the pinned SHA-256 in fetch-payload.sh proves we got the bytes we asked for
# and says nothing about who signed them. Same fail-closed check as OpenSSH.
# Wrapped, because $ErrorActionPreference is 'Stop' for this script: a throw in
# here would otherwise abort the bootstrap, and with it everything after it.
try {
"#,
    );
    s.push_str(&format!("  $msi = \"$root\\{MSI_MEDIA_SUBPATH}\"\n"));
    s.push_str(
        r#"  if (-not (Test-Path $msi)) {
    Log "WARNING: cloud-init was requested but no MSI is on this media"
  } else {
    $sig = Get-AuthenticodeSignature -FilePath $msi
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'O=Cloudbase Solutions') {
      Log "REFUSING to install cloud-init: [$($sig.Status)] $($sig.SignerCertificate.Subject)"
    } else {
      Log "cloud-init payload verified: signed by Cloudbase Solutions"
      $msiProc = Start-Process -FilePath 'msiexec.exe' -ArgumentList '/i',"`"$msi`"",'/qn','/norestart','RUN_SERVICE_AS_LOCAL_SYSTEM=1' -Wait -PassThru
      Log "msiexec exit $($msiProc.ExitCode)"
      $cbDir = "$env:ProgramFiles\Cloudbase Solutions\Cloudbase-Init"
      if (-not (Test-Path $cbDir)) { $cbDir = "${env:ProgramFiles(x86)}\Cloudbase Solutions\Cloudbase-Init" }
      if (-not (Test-Path "$cbDir\conf")) {
        Log "WARNING: cloud-init did not install: there is no $cbDir\conf"
      } else {
        # The confs name mtools_path as a fixed directory, because the file they
        # replace is where the installer would have written it. Without mtools
        # a vFAT config drive cannot be read and every clone ends "No metadata
        # service found" -- so say so here, where it is still a build problem.
"#,
    );
    s.push_str(&format!("        $mtools = {}\n", ps_quote(MTOOLS_PATH)));
    s.push_str(
        r#"        if (-not (Test-Path "$cbDir\bin\mlabel.exe") -or "$cbDir\bin\" -ne $mtools) {
          Log "WARNING: cloud-init: the confs set mtools_path=$mtools but cloudbase-init is at $cbDir (mlabel.exe present: $(Test-Path "$cbDir\bin\mlabel.exe")); the config drive will not be read"
        }
        # mtools' own sanity check refuses any FAT volume whose total sector
        # count is not a multiple of its declared sectors-per-track (63) --
        # the exact "mlabel failed with error ... not a multiple of sectors
        # per track" line in cloudbase-init-unattend.log. Our own config
        # drive (cidata.rs) is sized to pass; the control plane's is not ours
        # to size, so every drive cloudbase-init might be pointed at gets a
        # pass on the check. Machine-scoped so a service started at the next
        # boot inherits it -- before the clone's task runs cloudbase-init.
        [Environment]::SetEnvironmentVariable('MTOOLS_SKIP_CHECK', '1', 'Machine')
        Log "cloud-init: MTOOLS_SKIP_CHECK=1 set machine-wide"
"#,
    );
    s.push_str(&format!(
        "        foreach ($conf in \"$root\\{service_conf}\", \
         \"$root\\{unattend_conf}\") {{\n"
    ));
    s.push_str(
        r#"          if (Test-Path $conf) {
            Copy-Item -LiteralPath $conf -Destination (Join-Path "$cbDir\conf" (Split-Path -Leaf $conf)) -Force
            Log "cloud-init: installed $(Split-Path -Leaf $conf)"
          } else { Log "WARNING: cloud-init: $conf is missing from the media" }
        }
        # The installer drops an answer file of its own in conf\. It would run
        # instead of ours.
        $stock = "$cbDir\conf\Unattend.xml"
        if (Test-Path $stock) {
          Remove-Item -LiteralPath $stock -Force
          Log "cloud-init: removed the installer's own conf\Unattend.xml"
        }
      }
    }
  }
  # Disabled for the install, so it cannot contend with Setup's own passes.
  # OxideCloudInit puts it back once Setup has finished.
  $cbSvc = Get-Service -Name cloudbase-init -ErrorAction SilentlyContinue
  if ($cbSvc) {
    if ($cbSvc.Status -eq 'Running') { Stop-Service -Name cloudbase-init -Force -ErrorAction SilentlyContinue }
    Set-Service -Name cloudbase-init -StartupType Disabled
    Log "cloud-init: service disabled for the install"
  } else {
    Log "WARNING: no cloudbase-init service; cloud-init will not run"
  }
"#,
    );
    s.push_str(&format!(
        "  New-Item -ItemType Directory -Force -Path '{LOG_DIR}' | Out-Null\n"
    ));
    s.push_str(
        r#"} catch { Log "WARNING: cloud-init install failed: $_" }
"#,
    );

    if golden {
        s.push('\n');
        s.push_str(&clone_runner());
    }

    s.push('\n');
    s.push_str(&task_registration(config));
    s
}

/// Create [`OXIDE_DIR`] and replace its ACL. Left alone it inherits `C:\`'s,
/// which lets Authenticated Users modify anything created beneath it -- and a
/// golden image's clone-side runner lives there and is run as SYSTEM by every
/// clone's specialize pass: a user-writable script SYSTEM executes. So:
/// protected (nothing inherited from `C:\`), SYSTEM and Administrators full
/// control, Users read and execute, all three inherited by what is created
/// inside. The same shape the bootstrap gives `administrators_authorized_keys`.
/// First, so everything below is created under the new ACL; its own `try`, so
/// a failure is logged and the install goes on.
fn oxide_dir_acl() -> String {
    let mut s = String::new();
    s.push_str(
        r#"# The directory the cloud-init runner, its logs and the extras live in.
# C:\ grants Authenticated Users modify on what is created beneath it, and the
# clone-side runner in here is run as SYSTEM, so the inherited ACL has to go.
try {
"#,
    );
    s.push_str(&format!("  $oxDir = '{OXIDE_DIR}'\n"));
    s.push_str(
        r#"  New-Item -ItemType Directory -Force -Path $oxDir | Out-Null
  $oxAcl = Get-Acl -LiteralPath $oxDir
  $oxAcl.SetAccessRuleProtection($true, $false)
  $oxAcl.Access | ForEach-Object { $oxAcl.RemoveAccessRule($_) | Out-Null }
  # Names are localized (e.g. VORDEFINIERT\Administratoren) and translating
  # one throws on non-English Windows, so every principal here is a
  # well-known SID instead.
  foreach ($grant in @(
      @('S-1-5-18', 'FullControl'),
      @('S-1-5-32-544', 'FullControl'),
      @('S-1-5-32-545', 'ReadAndExecute'))) {
    $sid = New-Object System.Security.Principal.SecurityIdentifier($grant[0])
    $oxAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule(
      $sid, $grant[1], 'ContainerInherit,ObjectInherit', 'None', 'Allow'))) | Out-Null
  }
  Set-Acl -LiteralPath $oxDir -AclObject $oxAcl
  Log "${oxDir}: access limited to SYSTEM and Administrators; Users may read"
} catch { Log "WARNING: could not restrict access to ${oxDir}: $_" }
"#,
    );
    s
}

/// The clone-side runner at [`CLONE_SCRIPT_PATH`], and the bootstrap lines that
/// write it. Only a golden image has one: its only caller is the sysprep
/// answer file's `specialize` pass, and a named machine has no such file.
fn clone_runner() -> String {
    let conf = file_name(UNATTEND_CONF_VOLUME_PATH);
    let mut s = String::new();
    s.push_str(
        r#"# The clone-side runner. The sysprep answer file's specialize pass runs it
# once on each clone's first boot, against the unattend conf.
try {
"#,
    );
    s.push_str(&format!("  $runner = '{CLONE_SCRIPT_PATH}'\n"));
    s.push_str(
        r#"  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $runner) | Out-Null
  Set-Content -LiteralPath $runner -Encoding ASCII -Value @'
$ErrorActionPreference = 'Continue'
$log = "$env:SystemDrive\oxide-bootstrap.log"
function CLog($m) { "$(Get-Date -Format o)  cloud-init: $m" | Tee-Object -FilePath $log -Append }
"#,
    );
    s.push_str(&format!(
        "# {TASK_NAME} is registered disabled on the golden image, so it cannot race\n\
         # OxideGeneralize there. Only a clone's specialize pass runs this script, so\n\
         # this is where the task is armed.\n\
         try {{\n\
         \x20 Enable-ScheduledTask -TaskName '{TASK_NAME}' -ErrorAction Stop | Out-Null\n\
         \x20 CLog \"{TASK_NAME} enabled for this clone\"\n\
         }} catch {{ CLog \"WARNING: could not enable {TASK_NAME}: $_\" }}\n"
    ));
    s.push_str(
        r#"$cb = "$env:ProgramFiles\Cloudbase Solutions\Cloudbase-Init"
if (-not (Test-Path $cb)) { $cb = "${env:ProgramFiles(x86)}\Cloudbase Solutions\Cloudbase-Init" }
$exe = "$cb\Python\Scripts\cloudbase-init.exe"
"#,
    );
    s.push_str(&format!("$conf = \"$cb\\conf\\{conf}\"\n"));
    s.push_str(&format!("$ulog = '{LOG_DIR}\\{UNATTEND_LOG_FILE}'\n"));
    s.push_str(
        r#"if (-not (Test-Path $exe)) { CLog "no cloudbase-init at $exe; nothing to run"; exit 0 }
# Only this run's lines are searched below, so count what is there already.
$ulogSkip = 0
if (Test-Path -LiteralPath $ulog) { $ulogSkip = @(Get-Content -LiteralPath $ulog -ErrorAction SilentlyContinue).Count }
$code = $null
# The specialize pass may not have picked up the Machine MTOOLS_SKIP_CHECK
# set at install time (services and processes read Machine variables at
# their own next start, not this one's), so set it for this process too --
# same mtools "not a multiple of sectors per track" mlabel failure this
# guards against on the install side.
$env:MTOOLS_SKIP_CHECK = '1'
try {
  $proc = Start-Process -FilePath $exe -ArgumentList "--config-file","`"$conf`"" -Wait -PassThru -ErrorAction Stop
  $code = $proc.ExitCode
} catch { CLog "could not start cloudbase-init: $_" }
CLog "cloudbase-init exited $code"
# cloudbase-init exits 0 when it finds no metadata at all, so "exited 0" alone
# reads as success for a run that did nothing. Its own log says which it was.
# A warning only: the exit code below is unchanged.
try {
  if (Test-Path -LiteralPath $ulog) {
    $missed = Get-Content -LiteralPath $ulog -ErrorAction Stop | Select-Object -Skip $ulogSkip | Select-String -SimpleMatch 'No metadata service found' -Quiet
    if ($missed) { CLog "WARNING: cloudbase-init found no config drive; hostname, keys and user-data were not applied" }
  } else {
    CLog "WARNING: no $ulog to check whether cloudbase-init found a config drive"
  }
} catch { CLog "WARNING: could not read ${ulog}: $_" }
# Exit 1 on SUCCESS, deliberately. This command runs from the sysprep answer
# file's specialize pass with WillReboot=OnRequest, which reads an exit code of
# 1 as "reboot requested" -- and a run that has just changed the computer name
# needs exactly that. Failure is 2. This is the opposite of every other exit
# code in this codebase.
if ($code -eq 0) { exit 1 }
exit 2
'@
  Log "cloud-init: wrote $runner"
} catch { Log "WARNING: could not write the cloud-init runner: $_" }
"#,
    );
    s
}

/// The `OxideCloudInit` task script, and the bootstrap lines that write it and
/// register the task.
fn task_registration(config: &Config) -> String {
    let golden = config.generalize;
    let mut s = String::new();
    s.push_str(&format!(
        "# {TASK_NAME}: at every startup, once Setup has finished, bring the\n\
         # cloudbase-init service up -- in keep-my-account mode making the account's\n\
         # profile first. This task is the only thing that enables and starts the\n\
         # service, in both account modes. The install leaves it Disabled, so it can\n\
         # neither contend with Setup's own passes nor run on a golden image before\n\
         # it is generalized, and nothing else puts it back: the clone-side runner\n\
         # calls cloudbase-init.exe directly and leaves the service alone. Setting it\n\
         # to Automatic in the specialize pass would be too late anyway -- the SCM has\n\
         # started automatic services by then -- and would start it on the next boot\n\
         # before this task has made the profile the SSH-key plugin needs.\n"
    ));
    s.push_str(
        r#"try {
  $ciDir = "$env:SystemRoot\Setup\Scripts"
  $ciTask = "$ciDir\oxide-cloud-init.ps1"
  New-Item -ItemType Directory -Force -Path $ciDir | Out-Null
  Set-Content -LiteralPath $ciTask -Encoding ASCII -Value @'
$ErrorActionPreference = 'Continue'
$log = "$env:SystemDrive\oxide-bootstrap.log"
function TLog($m) {
  "$(Get-Date -Format o)  cloud-init task: $m" | Tee-Object -FilePath $log -Append
}
# First, before anything that can fail. On the first QEMU clone this task's
# Last Result was 0xC0000005 (-1073741819, an access violation) at the first
# startup after specialize, with nothing logged; run by hand later it was fine.
# powershell.exe died at early boot. This line tells "crashed after starting"
# from "never got this far".
TLog "starting"
# This task never unregisters itself and re-arms nothing: every step below is
# idempotent, so it simply runs at every startup. Generalizing gives each clone
# a new SID, so the profile it makes has to be made again on every clone -- an
# "unregister when done" edit would work on the golden image and silently break
# every machine cloned from it.
"#,
    );
    if golden {
        s.push_str(&format!(
            "# A golden image: before it is generalized there is nothing to do, and it\n\
             # shuts down straight after. {GENERALIZED_MARKER} is written just before the\n\
             # generalize and captured into the image, so it is there only on a clone.\n\
             if (-not (Test-Path \"$env:SystemDrive\\{GENERALIZED_MARKER}\")) {{\n\
             \x20 TLog \"not generalized yet; nothing to do until this image is cloned\"\n\
             \x20 exit 0\n\
             }}\n"
        ));
    }
    s.push_str(
        r#"# The task fires at every startup, including boots where Setup is still
# working. Do nothing until it has finished and let the next boot try again.
$setup = Get-ItemProperty -Path 'HKLM:\SYSTEM\Setup' -ErrorAction SilentlyContinue
if ($setup.SystemSetupInProgress -ne 0 -or $setup.OOBEInProgress -ne 0) {
  TLog "Setup still in progress; waiting for the next boot"
  exit 0
}
"#,
    );
    if !config.cloud_init.is_some_and(|c| c.manage_account) {
        s.push_str(&profile_step(config));
    }
    s.push_str(
        r#"if (-not (Get-Service -Name cloudbase-init -ErrorAction SilentlyContinue)) {
  TLog "WARNING: no cloudbase-init service; nothing to start"
  exit 0
}
try {
  Set-Service -Name cloudbase-init -StartupType Automatic -ErrorAction Stop
  TLog "cloudbase-init set to start automatically"
} catch { TLog "WARNING: could not set cloudbase-init to Automatic: $_" }
if ((Get-Service -Name cloudbase-init).Status -eq 'Running') {
  TLog "cloudbase-init already running"
} else {
  try {
    Start-Service -Name cloudbase-init -ErrorAction Stop
    TLog "cloudbase-init started"
  } catch { TLog "WARNING: could not start cloudbase-init: $_" }
}
exit 0
'@
  # Wrapped for the same reason OxideGeneralize is: a failed registration must
  # not abort the bootstrap without saying why.
  $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$ciTask`""
  $trigger = New-ScheduledTaskTrigger -AtStartup
  # A minute's delay, and three retries a minute apart. At the first startup
  # after a clone's specialize pass powershell.exe crashed with 0xC0000005
  # before this script logged a line, and a manual run later worked: early
  # boot, not the script. The delay is the fix; the retries are a second line,
  # since nothing here establishes that Task Scheduler counts a crashed
  # process's exit code as a failure to restart on.
  $trigger.Delay = 'PT1M'
  $settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
  $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
"#,
    );
    s.push_str(&format!(
        "  Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action \
         -Trigger $trigger -Settings $settings -Principal $principal -Force \
         | Out-Null\n"
    ));
    if golden {
        s.push_str(&format!(
            "  # Disabled on a golden image. OxideGeneralize is a startup task too and\n\
             \x20 # starts alongside this one; it writes {GENERALIZED_MARKER} and then runs\n\
             \x20 # sysprep, so the marker check alone would let this task make a profile\n\
             \x20 # and start the service on the golden image mid-sysprep. The clone-side\n\
             \x20 # runner, which only a clone's specialize pass runs, enables it.\n\
             \x20 Disable-ScheduledTask -TaskName '{TASK_NAME}' | Out-Null\n\
             \x20 Log \"cloud-init: {TASK_NAME} registered, disabled until a clone's specialize pass enables it\"\n"
        ));
    } else {
        s.push_str(&format!(
            "  Log \"cloud-init: {TASK_NAME} registered; it starts cloud-init once \
             Setup has finished\"\n"
        ));
    }
    s.push_str(&format!(
        "}} catch {{ Log \"WARNING: could not register {TASK_NAME}, so cloud-init \
         will not start on its own: $_\" }}\n"
    ));
    s
}

/// Mode A's profile step, inside the task script. Mode B leaves the profile to
/// `CreateUserPlugin`.
fn profile_step(config: &Config) -> String {
    let mut s = String::new();
    s.push_str(
        r#"# Keep-my-account mode. SetUserSSHPublicKeysPlugin writes to the account's
# profile, which it finds through ProfileList\<SID>\ProfileImagePath, and an
# account an answer file created has no profile until someone logs on. Without
# one the plugin raises "User profile not found!" and the instance's keys are
# silently absent. So make it, before the service runs.
"#,
    );
    s.push_str(&format!("$user = {}\n", ps_quote(&config.username)));
    // No password, deliberately. This script lives in
    // C:\Windows\Setup\Scripts, inherits BUILTIN\Users read access, is never
    // deleted, and is captured into every clone -- on a named build it would
    // be the only cleartext copy left on disk, since there is no sysprep
    // answer file and Windows scrubs its cached one. The only thing a password
    // could feed is Start-Process -Credential, which is CreateProcessWithLogonW
    // and documented not to work from LocalSystem, which is what this task
    // runs as. userenv's CreateProfile needs none.
    s.push_str(
        r#"$sid = $null
try {
  $sid = (New-Object System.Security.Principal.NTAccount($user)).Translate([System.Security.Principal.SecurityIdentifier]).Value
} catch { TLog "WARNING: no account named $user to make a profile for: $_" }
if ($sid) {
  $profileKey = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$sid"
  $profilePath = (Get-ItemProperty -Path $profileKey -ErrorAction SilentlyContinue).ProfileImagePath
  # C:\Users\TEMP (or TEMP.<something>) is the temporary profile Windows hands
  # out when it cannot load the real one, and it is thrown away at logoff:
  # keys written there vanish. Seen on the first QEMU clone, probably from a
  # console logon on the golden before it was generalized. Not a profile,
  # then -- and the service is not started this boot, so the next one can
  # try again.
  if ($profilePath -match '\\Users\\TEMP(\.[^\\]*)?\\?$') {
    TLog "WARNING: the profile for $user is a temporary one at $profilePath; not starting cloudbase-init this boot, because keys written there are lost"
    exit 0
  }
  if ($profilePath) {
    TLog "profile for $user already exists at $profilePath"
  } else {
    TLog "no profile for $user; making one so cloud-init can write its SSH keys"
    # userenv's CreateProfile makes the ProfileList entry and the profile
    # directory, needs no password, and works from LocalSystem. Logging on as
    # the user to load its profile does not: that is CreateProcessWithLogonW,
    # documented not to work from LocalSystem.
    try {
      Add-Type -Namespace Oxide -Name UserEnv -MemberDefinition '[DllImport("userenv.dll", CharSet = CharSet.Unicode)] public static extern int CreateProfile(string sid, string name, System.Text.StringBuilder path, uint len);' -ErrorAction Stop
      $buf = New-Object System.Text.StringBuilder 260
      $hr = [Oxide.UserEnv]::CreateProfile($sid, $user, $buf, 260)
      TLog ('CreateProfile returned 0x{0:X8}' -f $hr)
    } catch { TLog "CreateProfile failed: $_" }
    $profilePath = (Get-ItemProperty -Path $profileKey -ErrorAction SilentlyContinue).ProfileImagePath
    if ($profilePath) {
      TLog "profile for $user made at $profilePath"
    } else {
      TLog "WARNING: still no profile for $user; the instance's SSH keys will not be written"
    }
  }
}
"#,
    );
    s
}

/// The .NET regex that finds the administrators-only directive.
///
/// It ends `(?=\r?$)`, not `$`: in .NET, `(?m)$` matches only before `\n`,
/// never before `\r`, and the `sshd_config_default` OpenSSH ships -- which
/// sshd copies verbatim to `sshd_config` on first start -- is CRLF. With a
/// bare `$` the rewrite never matched on a real guest, the warning was logged
/// on every install, and the metadata keys silently did nothing while the
/// baked keys hid it. The lookahead leaves the `\r` in place rather than
/// consuming it. Pinned against both line endings by the fixtures in
/// `testdata/sshd/`.
const SSHD_KEYS_PATTERN: &str = r"(?m)^[ \t]*#?[ \t]*AuthorizedKeysFile[ \t]+__PROGRAMDATA__/ssh/administrators_authorized_keys[ \t]*(?=\r?$)";

/// The `AuthorizedKeysFile` rewrite. Emitted inside `bootstrap`'s
/// `enable_ssh` branch, inside its `if (Get-Service sshd)` block, and only when
/// cloud-init is on: with it off there are no per-user keys to read.
pub fn sshd_fix_block() -> String {
    SSHD_FIX_BLOCK.replace("@PATTERN@", SSHD_KEYS_PATTERN)
}

const SSHD_FIX_BLOCK: &str = r#"  # Stock sshd_config ends with `Match Group administrators`, which points
  # AuthorizedKeysFile at administrators_authorized_keys only -- so an
  # administrator's own ~/.ssh/authorized_keys is ignored, and the per-instance
  # keys cloud-init writes there do nothing. Name both files rather than
  # deleting the block: the block is where the baked keys live.
  # Wrapped, because 'Stop' is in force and nothing here is worth the rest of
  # the bootstrap: an empty file reads as $null, and Replace throws on $null.
  try {
    $sshdConf = "$env:ProgramData\ssh\sshd_config"
    if (Test-Path $sshdConf) {
      $want = 'AuthorizedKeysFile .ssh/authorized_keys __PROGRAMDATA__/ssh/administrators_authorized_keys'
      $text = Get-Content -LiteralPath $sshdConf -Raw
      $new = [regex]::Replace($text, '@PATTERN@', $want)
      if ($new -ne $text) {
        Set-Content -LiteralPath $sshdConf -Value $new -Encoding ascii
        Log "sshd_config: AuthorizedKeysFile now names both key files"
        if (Get-Service -Name sshd -ErrorAction SilentlyContinue) { Restart-Service sshd -ErrorAction SilentlyContinue }
      } elseif ($text -and [regex]::IsMatch($text, '(?m)^[ \t]*' + [regex]::Escape($want) + '[ \t]*(?=\r?$)')) {
        # A second run: a clone's specialize pass re-runs this script when the
        # installer is left attached, and the first run already rewrote it.
        Log "sshd_config: AuthorizedKeysFile already names both key files"
      } else {
        Log "WARNING: sshd_config has no administrators_authorized_keys directive to rewrite; per-instance keys may be ignored"
      }
    } else {
      Log "WARNING: no sshd_config; per-instance keys will not be read"
    }
  } catch { Log "WARNING: could not rewrite sshd_config; per-instance keys may be ignored: $_" }"#;

/// Copy `\extras` on the media to `C:\oxide\extras`, preserving the relative
/// structure and logging every file, so `C:\oxide-bootstrap.log` answers "did
/// my file make it?" without getting into the guest. Nothing is run: running
/// something is what `user_data` is for, per instance rather than per image.
pub fn extras_block() -> String {
    r#"# Extra files from the media. Copied, never run.
$extrasSrc = "$root\extras"
$extrasDst = "$env:SystemDrive\oxide\extras"
if (Test-Path -LiteralPath $extrasSrc) {
  try {
    New-Item -ItemType Directory -Force -Path $extrasDst | Out-Null
    $extrasBase = (Get-Item -LiteralPath $extrasSrc).FullName.TrimEnd('\')
    $extrasCount = 0
    Get-ChildItem -LiteralPath $extrasSrc -Recurse -File -Force | ForEach-Object {
      $rel = $_.FullName.Substring($extrasBase.Length + 1)
      $dst = Join-Path $extrasDst $rel
      New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
      Copy-Item -LiteralPath $_.FullName -Destination $dst -Force
      Log "extras: $rel ($($_.Length) bytes)"
      $extrasCount++
    }
    Log "extras: copied $extrasCount file(s) to $extrasDst"
  } catch { Log "WARNING: copying the extras failed: $_" }
} else {
  Log "WARNING: extras were requested but $extrasSrc is not on the media"
}
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::CloudInit;
    use crate::settings::WindowsRelease;
    use std::path::PathBuf;

    fn base() -> Config {
        Config {
            release: WindowsRelease::Server2022,
            edition: "datacenter".into(),
            computer_name: "*".into(),
            username: "oxide".into(),
            password: "0xide!230xide!23".into(),
            enable_rdp: true,
            inject_drivers: true,
            enable_serial_console: true,
            target_disk: 1,
            partitions: crate::partition::default_layout(),
            ui_language: "en-US".into(),
            region: "en-US".into(),
            timezone: "UTC".into(),
            product_key: None,
            auto_logon: false,
            generalize: true,
            verbose_serial: false,
            log_path: None,
            show_ui_on_error: true,
            image_index: Some(4),
            skip_image_install: false,
            ssh_keys: Vec::new(),
            enable_ssh: true,
            cloud_init: Some(CloudInit { manage_account: false }),
            has_extras: false,
        }
    }

    /// The stock `metadata_services` list probes HTTP metadata endpoints that do
    /// not exist on a rack, and a guest with no route to them spends its first
    /// boot timing out. Exactly one service, in both files.
    #[test]
    fn exactly_one_metadata_service() {
        for conf in [service_conf(&base()), unattend_conf(&base())] {
            let lines: Vec<&str> = conf
                .lines()
                .filter(|l| l.starts_with("metadata_services="))
                .collect();
            assert_eq!(lines.len(), 1, "{conf}");
            assert_eq!(
                lines[0],
                "metadata_services=cloudbaseinit.metadata.services.nocloudservice.NoCloudConfigDriveService"
            );
        }
    }

    /// A named deployment is a machine whose name the user typed. Cloud-init has
    /// no business overwriting it, so the plugin is absent from both files.
    #[test]
    fn a_named_deployment_gets_no_hostname_plugin() {
        let named = Config {
            computer_name: "win-server-01".into(),
            generalize: false,
            ..base()
        };
        for conf in [service_conf(&named), unattend_conf(&named)] {
            assert!(!conf.contains("SetHostNamePlugin"), "{conf}");
        }
        // And a golden image does get it: `*` covers a missing config drive,
        // the plugin overwrites it with the instance's name when there is one.
        for conf in [service_conf(&base()), unattend_conf(&base())] {
            assert!(conf.contains("SetHostNamePlugin"), "{conf}");
        }
    }

    /// Mode A must not carry `CreateUserPlugin`: it resets the password to a
    /// per-instance random one on every path, including the one for an account
    /// that already exists.
    #[test]
    fn only_mode_b_creates_the_user() {
        assert!(!service_conf(&base()).contains("CreateUserPlugin"));
        let managed = Config {
            cloud_init: Some(CloudInit { manage_account: true }),
            ..base()
        };
        assert!(service_conf(&managed).contains("CreateUserPlugin"));
    }

    /// Both user-data shapes have to work: `#cloud-config` YAML and a
    /// `<powershell>`/`<script>` blob.
    #[test]
    fn both_user_data_plugins_are_configured() {
        let conf = service_conf(&base());
        assert!(conf.contains("cloudconfig.CloudConfigPlugin"));
        assert!(conf.contains("shellscript.ShellScriptPlugin"));
    }

    /// RDP keepalive and the RDP plugin are conditional on RDP being on; with
    /// it off they are absent rather than set to false.
    #[test]
    fn rdp_settings_follow_the_rdp_switch() {
        let off = Config { enable_rdp: false, ..base() };
        for conf in [service_conf(&off), unattend_conf(&off)] {
            assert!(!conf.contains("RDPSettingsPlugin"), "{conf}");
            assert!(!conf.contains("rdp_set_keepalive"), "{conf}");
        }
        assert!(unattend_conf(&base()).contains("rdp_set_keepalive=true"));
        assert!(unattend_conf(&base()).contains("RDPSettingsPlugin"));
    }

    /// Everything this tool leaves in a guest is in one place, which is what
    /// makes a machine reachable only over a serial console debuggable.
    #[test]
    fn logs_land_under_the_oxide_directory() {
        assert!(service_conf(&base()).contains(r"log_dir=C:\oxide\log"));
        assert!(service_conf(&base()).contains("log_file=cloudbase-init.log"));
        assert!(
            unattend_conf(&base())
                .contains("log_file=cloudbase-init-unattend.log")
        );
    }

    /// The prototype wrote debug chatter to COM3. An Oxide instance surfaces
    /// COM1, which carries SAC and EMS, and nothing establishes COM3 exists at
    /// all -- so that line was writing into the void. Files only.
    #[test]
    fn nothing_is_logged_to_a_serial_port() {
        for conf in [service_conf(&base()), unattend_conf(&base())] {
            for port in ["COM1", "COM2", "COM3", "COM4"] {
                assert!(!conf.contains(port), "{conf}");
            }
            assert!(!conf.contains("logging_serial_port"), "{conf}");
        }
    }

    /// Turning automatic updates on across somebody's fleet is a policy
    /// decision disguised as a default, and this tool has no update setting.
    #[test]
    fn automatic_updates_are_left_to_windows() {
        for conf in [service_conf(&base()), unattend_conf(&base())] {
            assert!(!conf.contains("WindowsAutoUpdates"), "{conf}");
            assert!(!conf.contains("enable_automatic_updates"), "{conf}");
        }
    }

    /// Read by Python on Windows off a volume that may be mounted read-only,
    /// like every other generated file here.
    #[test]
    fn the_confs_are_crlf_and_ascii() {
        for conf in [service_conf(&base()), unattend_conf(&base())] {
            assert!(conf.is_ascii(), "{conf}");
            assert_eq!(
                conf.matches('\n').count(),
                conf.matches("\r\n").count()
            );
        }
    }

    /// The pinned SHA-256 proves we got the bytes we asked for. It says nothing
    /// about who signed them, so the signature is checked on the machine
    /// itself, and the refusal comes before the install -- the same shape the
    /// script already uses for OpenSSH.
    #[test]
    fn the_msi_is_only_installed_when_cloudbase_signed_it() {
        let script = install_block(&base());
        assert!(script.contains("Get-AuthenticodeSignature"));
        assert!(script.contains("O=Cloudbase Solutions"));
        let refuse =
            script.find("REFUSING to install cloud-init").expect("a refusal");
        let install = script.find("msiexec").expect("the install");
        assert!(refuse < install, "the install happens before the check");
    }

    #[test]
    fn the_service_is_disabled_for_the_install_and_started_by_the_task() {
        let script = install_block(&base());
        assert!(script.contains("-StartupType Disabled"));
        assert!(script.contains(TASK_NAME));
        assert!(script.contains("Register-ScheduledTask"));
    }

    /// The installer drops its own Unattend.xml in conf\, which would run
    /// instead of ours.
    #[test]
    fn the_installers_own_conf_is_removed() {
        assert!(install_block(&base()).contains("Unattend.xml"));
    }

    /// Mode A materialises the profile itself, because without one
    /// `get_user_home` raises `User profile not found!` and the metadata keys
    /// are simply absent. Mode B lets `CreateUserPlugin` do it.
    #[test]
    fn only_mode_a_materialises_the_profile() {
        let keep = install_block(&base());
        assert!(keep.contains("CreateProfile("));
        let managed = install_block(&Config {
            cloud_init: Some(CloudInit { manage_account: true }),
            ..base()
        });
        assert!(!managed.contains("CreateProfile"));
        // Both still register the task: it is what starts the service on the
        // clone's *first* boot rather than its second.
        assert!(managed.contains(TASK_NAME));
    }

    /// Success is exit code 1, because WillReboot=OnRequest reads 1 as "reboot
    /// requested" and a run that changed the hostname needs exactly that. This
    /// is the opposite of every other exit code here, so it is asserted rather
    /// than left to be "fixed" by the next reader.
    #[test]
    fn the_clone_runner_reports_success_as_one() {
        let script = install_block(&base());
        assert!(script.contains(CLONE_SCRIPT_PATH));
        assert!(
            script.contains("exit 1"),
            "success must map to 1: WillReboot=OnRequest reads it as a reboot request"
        );
        assert!(script.contains("exit 2"));
        // And it is the zero exit code that maps to 1, not any exit code.
        assert!(script.contains("if ($code -eq 0) { exit 1 }"));
        // From the process object: cloudbase-init.exe is a console program,
        // but the sysprep lesson is not worth relearning per program.
        assert!(script.contains("-Wait -PassThru"));
        assert!(!script.contains("$code = $LASTEXITCODE"));
    }

    /// The body of the single-quoted here-string written to `target` (the
    /// PowerShell variable the bootstrap writes it through).
    fn here_string<'a>(script: &'a str, target: &str) -> &'a str {
        let opener = format!(
            "Set-Content -LiteralPath {target} -Encoding ASCII -Value @'\n"
        );
        let start = script.find(&opener).expect("the here-string opener")
            + opener.len();
        let len = script[start..].find("\n'@").expect("its closer");
        &script[start..start + len]
    }

    /// Finding 1 of the first QEMU run: with no `mtools_path`, 1.1.8's
    /// `vfat.py` refuses to read a vFAT drive, the search swallows that, and
    /// the clone gets no hostname, keys or user_data. The installer's conf
    /// sets it; ours replaces that conf, so both of ours must.
    #[test]
    fn both_confs_name_mtools() {
        let want = r"mtools_path=C:\Program Files\Cloudbase Solutions\Cloudbase-Init\bin\";
        for config in [base(), Config { enable_rdp: false, ..base() }] {
            for conf in [service_conf(&config), unattend_conf(&config)] {
                let lines: Vec<&str> = conf
                    .lines()
                    .filter(|l| l.starts_with("mtools_path="))
                    .collect();
                assert_eq!(lines, [want], "{conf}");
                // In [DEFAULT], not [config_drive]: oslo reads it from there.
                let at = conf.find(want).unwrap();
                assert!(at < conf.find("[config_drive]").unwrap(), "{conf}");
            }
        }
        // And the bootstrap says so if the install is not where that names.
        let script = install_block(&base());
        assert!(script.contains(r#"Test-Path "$cbDir\bin\mlabel.exe""#));
        assert!(script.contains(r#""$cbDir\bin\" -ne $mtools"#));
        assert!(script.contains(&format!("$mtools = '{MTOOLS_PATH}'")));
    }

    /// Task 12: mtools' own sanity check refuses any FAT volume whose total
    /// sector count is not a multiple of its declared sectors-per-track,
    /// which cloudbase-init's direct `mlabel` call cannot be told to skip
    /// except through mtools' own configuration. The install side sets the
    /// Machine environment variable, once, so a later boot's service run
    /// inherits it before the clone-side runner ever starts cloudbase-init.
    #[test]
    fn the_install_sets_mtools_skip_check_machine_wide() {
        let script = install_block(&base());
        assert!(script.contains(
            "[Environment]::SetEnvironmentVariable('MTOOLS_SKIP_CHECK', \
             '1', 'Machine')"
        ));
        assert!(script.contains("MTOOLS_SKIP_CHECK=1 set machine-wide"));
        // Inside the same guarded try/catch as the rest of the MSI install,
        // so a failure here logs a warning rather than aborting the
        // bootstrap under $ErrorActionPreference = 'Stop'.
        let set = script.find("SetEnvironmentVariable").unwrap();
        let try_start = script.find("try {\n").unwrap();
        let catch = script
            .find(r#"} catch { Log "WARNING: cloud-init install failed"#)
            .unwrap();
        assert!(try_start < set && set < catch, "{script}");
    }

    /// The clone runner cannot rely on the Machine variable the install set:
    /// the specialize pass that runs it may not have picked it up yet. It
    /// sets the process variable for itself before calling cloudbase-init.exe,
    /// so `Start-Process`'s child inherits it either way.
    #[test]
    fn the_runner_sets_mtools_skip_check_before_running_cloudbase_init() {
        let script = install_block(&base());
        let runner = here_string(&script, "$runner");
        assert!(runner.contains("$env:MTOOLS_SKIP_CHECK = '1'"));
        let set = runner.find("$env:MTOOLS_SKIP_CHECK").unwrap();
        let run = runner.find("Start-Process -FilePath $exe").unwrap();
        assert!(set < run, "{runner}");
    }

    /// Finding 2 asked for `raw_hdd`/`cdrom` to go, for their deprecation
    /// warnings. They stay: either one left to its default `true` puts `iso`
    /// back in the types, and `hdd` x `iso` is a probe that checks no label.
    /// See `config_drive_block`.
    #[test]
    fn the_config_drive_search_is_narrowed_to_vfat_on_a_disk() {
        for conf in [service_conf(&base()), unattend_conf(&base())] {
            let block = &conf[conf.find("[config_drive]\r\n").unwrap()..];
            assert_eq!(
                block,
                "[config_drive]\r\ntypes=vfat\r\nlocations=hdd\r\n\
                 raw_hdd=false\r\ncdrom=false\r\n"
            );
        }
    }

    /// Finding 3: `OxideCloudInit` died with 0xC0000005 at the first startup
    /// after specialize, before logging anything. A delay and retries, and a
    /// log line before anything that can fail.
    #[test]
    fn the_task_waits_retries_and_logs_first() {
        for config in [
            base(),
            Config {
                cloud_init: Some(CloudInit { manage_account: true }),
                ..base()
            },
            Config {
                generalize: false,
                computer_name: "win-server-01".into(),
                ..base()
            },
        ] {
            let script = install_block(&config);
            assert!(script.contains("  $trigger.Delay = 'PT1M'\n"), "{script}");
            assert!(script.contains(
                "  $settings = New-ScheduledTaskSettingsSet -RestartCount 3 \
                 -RestartInterval (New-TimeSpan -Minutes 1)\n"
            ));
            let register = script.find("Register-ScheduledTask").unwrap();
            assert!(script.find("$trigger.Delay").unwrap() < register);
            assert!(
                script[register..].starts_with(
                    "Register-ScheduledTask -TaskName 'OxideCloudInit' \
                     -Action $action -Trigger $trigger -Settings $settings "
                ),
                "{script}"
            );

            // The first statement after the logger is the log line: nothing
            // that can fail -- Add-Type, a registry read, a Test-Path -- runs
            // before it.
            let task = here_string(&script, "$ciTask");
            let first = task.find("\nTLog \"starting\"\n").expect("starting");
            let logger_end = task.find("\n}\n").unwrap() + 3;
            assert!(logger_end <= first + 1, "{task}");
            let between: Vec<&str> = task[logger_end..first]
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                .collect();
            assert!(between.is_empty(), "before the first log: {between:?}");
            for later in ["Add-Type", "Get-ItemProperty", "Test-Path"] {
                if let Some(at) = task.find(later) {
                    assert!(first < at, "{later} precedes the first log");
                }
            }
        }
    }

    /// Finding 4: `C:\Users\TEMP` is a temporary profile, discarded at
    /// logoff. Keys written there are lost, so it counts as no profile, and
    /// the service is left for the next boot.
    #[test]
    fn a_temporary_profile_is_not_a_profile() {
        let task = install_block(&base());
        let check =
            r"if ($profilePath -match '\\Users\\TEMP(\.[^\\]*)?\\?$') {";
        let at = task.find(check).expect("the TEMP check");
        let warn =
            task.find("WARNING: the profile for $user is a temporary").unwrap();
        let exists = task.find("already exists at $profilePath").unwrap();
        let start = task.find("Start-Service -Name cloudbase-init").unwrap();
        assert!(at < warn && warn < exists && exists < start);
        // The branch leaves before the service is touched.
        assert!(task[warn..exists].contains("exit 0"));
        // Mode B has no profile step at all.
        let managed = install_block(&Config {
            cloud_init: Some(CloudInit { manage_account: true }),
            ..base()
        });
        assert!(!managed.contains("TEMP"));
    }

    /// Finding 5: cloudbase-init exits 0 having found no metadata, so the
    /// runner reads its log and says so. The exit codes do not move.
    #[test]
    fn the_runner_says_when_no_config_drive_was_found() {
        let script = install_block(&base());
        let runner = here_string(&script, "$runner");
        assert!(
            runner.contains(
                r"$ulog = 'C:\oxide\log\cloudbase-init-unattend.log'"
            )
        );
        assert!(runner.contains(
            "Select-String -SimpleMatch 'No metadata service found' -Quiet"
        ));
        assert!(runner.contains(
            "CLog \"WARNING: cloudbase-init found no config drive; \
             hostname, keys and user-data were not applied\""
        ));
        // Searched after the run, and only this run's lines.
        let run = runner.find("Start-Process -FilePath $exe").unwrap();
        let search = runner.find("No metadata service found").unwrap();
        assert!(run < search);
        assert!(runner.contains("Select-Object -Skip $ulogSkip"));
        // The log the runner reads is the log the conf writes.
        assert!(
            unattend_conf(&base())
                .contains(&format!("log_file={UNATTEND_LOG_FILE}\r\n"))
        );
        // The convention is untouched: success 1, failure 2.
        assert!(runner.ends_with("if ($code -eq 0) { exit 1 }\nexit 2"));
    }

    /// Off means off: no MSI, no conf, no task, nothing.
    #[test]
    fn cloud_init_off_emits_nothing() {
        let off = Config { cloud_init: None, ..base() };
        assert!(install_block(&off).is_empty());
    }

    /// Stock sshd_config ends with a `Match Group administrators` block that
    /// points AuthorizedKeysFile at administrators_authorized_keys only, so an
    /// administrator's per-user file is ignored and the metadata keys land
    /// where sshd will not read them. The prototype commented both directives
    /// out, which breaks the *baked* keys instead. Name both files.
    #[test]
    fn the_sshd_rewrite_names_both_key_files() {
        let block = sshd_fix_block();
        assert!(block.contains(
            ".ssh/authorized_keys __PROGRAMDATA__/ssh/administrators_authorized_keys"
        ));
        // Rewritten in place, never commented out or deleted: the Match block
        // is where the baked keys live. The prototype commented it out, which
        // broke them. The only write is the regex replace, whose replacement
        // is the directive naming both files -- no `#` goes into the file.
        assert_eq!(block.matches("Set-Content").count(), 1, "{block}");
        assert!(block.contains(
            "Set-Content -LiteralPath $sshdConf -Value $new -Encoding ascii"
        ));
        assert!(block.contains(&format!(
            "$new = [regex]::Replace($text, '{SSHD_KEYS_PATTERN}', $want)"
        )));
        let want = "$want = 'AuthorizedKeysFile .ssh/authorized_keys \
                    __PROGRAMDATA__/ssh/administrators_authorized_keys'";
        assert!(block.contains(want), "{block}");
        for gone in ["-replace", "Remove-Item", "'#", "\"#"] {
            assert!(!block.contains(gone), "{gone} in {block}");
        }
        // And a missing file is a logged warning, never a throw: the script
        // runs under $ErrorActionPreference = 'Stop'.
        assert!(block.contains("Test-Path"));
        // The pattern is what is emitted, whole.
        assert!(block.contains(&format!("'{SSHD_KEYS_PATTERN}'")), "{block}");
        assert!(!block.contains("@PATTERN@"));
    }

    /// With the installer left attached, a clone's specialize pass re-runs
    /// the bootstrap, and the first run has already rewritten the directive,
    /// so the pattern no longer matches. That is success, not the "no
    /// directive" warning -- which is the tell of the CRLF bug and must not
    /// be logged on a guest that is fine.
    #[test]
    fn a_second_run_of_the_sshd_rewrite_is_not_a_warning() {
        let block = sshd_fix_block();
        let changed = block.find("if ($new -ne $text) {").unwrap();
        let already = block
            .find("} elseif ($text -and [regex]::IsMatch($text, '(?m)^[ \\t]*' + [regex]::Escape($want) + '[ \\t]*(?=\\r?$)')) {")
            .expect("the already-rewritten branch");
        let logged = block
            .find("Log \"sshd_config: AuthorizedKeysFile already names both key files\"")
            .unwrap();
        let warning = block
            .find("WARNING: sshd_config has no administrators_authorized_keys")
            .unwrap();
        assert!(changed < already && already < logged && logged < warning);
        // The warning is the else of that branch, not a separate check.
        assert!(block[logged..warning].contains("} else {"));
    }

    /// In .NET, `(?m)$` matches before `\n` only, never before `\r`, and the
    /// sshd_config_default OpenSSH ships is CRLF: a bare `$` never matched on
    /// a guest. The line end is a lookahead, so the `\r` is kept, not eaten.
    /// Behaviour against the fixtures is checked with pwsh (see the task-3
    /// report); what can be held here is the shape.
    #[test]
    fn the_sshd_pattern_matches_crlf_line_ends() {
        assert!(SSHD_KEYS_PATTERN.starts_with("(?m)^"));
        assert!(
            SSHD_KEYS_PATTERN.ends_with(r"[ \t]*(?=\r?$)"),
            "{SSHD_KEYS_PATTERN}"
        );
        // No bare `$` anywhere else in it.
        assert_eq!(SSHD_KEYS_PATTERN.matches('$').count(), 1);
        // A single-quoted PowerShell string cannot carry a quote unescaped.
        assert!(!SSHD_KEYS_PATTERN.contains('\''));
    }

    /// The fixtures are the last lines of the real sshd_config_default in the
    /// bundled OpenSSH-Win64.zip, verbatim (CRLF), and the same lines as LF.
    /// Held here so they cannot quietly lose the thing they exist to test.
    #[test]
    fn the_sshd_fixtures_carry_the_directive_in_both_line_endings() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/sshd");
        let directive = "AuthorizedKeysFile __PROGRAMDATA__/ssh/administrators_authorized_keys";
        let crlf = std::fs::read_to_string(
            dir.join("sshd_config_default-tail-crlf.txt"),
        )
        .expect("CRLF fixture");
        let lf = std::fs::read_to_string(
            dir.join("sshd_config_default-tail-lf.txt"),
        )
        .expect("LF fixture");
        assert!(crlf.contains("Match Group administrators\r\n"));
        assert!(crlf.contains(&format!("       {directive}\r\n")));
        assert_eq!(crlf.matches('\n').count(), crlf.matches("\r\n").count());
        assert!(!lf.contains('\r'));
        assert_eq!(lf, crlf.replace("\r\n", "\n"));
    }

    #[test]
    fn the_extras_copy_logs_every_file() {
        let block = extras_block();
        assert!(block.contains(r"$env:SystemDrive\oxide\extras"));
        assert!(block.contains("Log \"extras:"));
        // No execution, ever. The user-data path is where running something
        // lives, per instance rather than per image.
        for forbidden in
            ["Invoke-Expression", "Start-Process", "& $", "setup.ps1"]
        {
            assert!(!block.contains(forbidden), "{forbidden} in extras block");
        }
    }

    #[test]
    fn the_guest_blocks_are_ascii() {
        for block in [install_block(&base()), sshd_fix_block(), extras_block()]
        {
            assert!(block.is_ascii(), "{block}");
        }
    }

    /// One fact, two spellings: the builder writes the MSI at a volume path
    /// and the guest reads it at a path under `$root`. `VOLUME_LABEL` once
    /// drifted between two places exactly like this, and the bootstrap was
    /// skipped on every install while installs appeared to succeed.
    #[test]
    fn the_guest_reads_the_files_where_the_builder_writes_them() {
        assert_eq!(media_subpath(MSI_VOLUME_PATH), MSI_MEDIA_SUBPATH);
        let script = install_block(&base());
        assert!(script.contains(r"$root\cloudbase\CloudbaseInitSetup_x64.msi"));
        for conf in [SERVICE_CONF_VOLUME_PATH, UNATTEND_CONF_VOLUME_PATH] {
            let sub = media_subpath(conf);
            assert!(sub.starts_with(r"cloudbase\"), "{sub}");
            assert!(script.contains(&format!(r"$root\{sub}")), "{sub}");
        }
        // The runner points cloudbase-init at the unattend conf by the name
        // it was copied under.
        assert!(script.contains(r"$cb\conf\cloudbase-init-unattend.conf"));
    }

    /// The account name is spliced into the task script as a PowerShell
    /// single-quoted string, so it must survive a quote, or the task script
    /// does not parse and no clone ever starts cloud-init.
    #[test]
    fn a_name_containing_a_quote_is_escaped() {
        let script =
            install_block(&Config { username: "o'brien".into(), ..base() });
        assert!(script.contains("$user = 'o''brien'"), "{script}");
        assert!(!script.contains("'o'brien'"));
    }

    /// The task script lives in C:\Windows\Setup\Scripts, is readable by
    /// BUILTIN\Users, is never deleted and is captured into every clone. So
    /// the password is in nothing this module generates for the guest, in
    /// either mode, golden or named, escaped or not. (It is in the answer
    /// files, which is where it belongs.)
    #[test]
    fn the_password_is_never_written_by_the_guest_blocks() {
        for password in [base().password, "it's-a-secret".to_string()] {
            let escaped = password.replace('\'', "''");
            for manage_account in [false, true] {
                for generalize in [true, false] {
                    let config = Config {
                        password: password.clone(),
                        generalize,
                        computer_name: if generalize {
                            "*".into()
                        } else {
                            "win-server-01".into()
                        },
                        cloud_init: Some(CloudInit { manage_account }),
                        ..base()
                    };
                    for out in [
                        install_block(&config),
                        sshd_fix_block(),
                        extras_block(),
                    ] {
                        assert!(!out.contains(&password), "{out}");
                        assert!(!out.contains(&escaped), "{out}");
                        assert!(!out.contains("$password"), "{out}");
                        assert!(!out.contains("-Credential"), "{out}");
                    }
                }
            }
        }
    }

    /// The task script and the runner are written from inside bootstrap.ps1
    /// as single-quoted here-strings. Those do not nest, and a line starting
    /// `'@` inside one ends it early -- after which the rest of the script is
    /// parsed as bootstrap code. Every opener has exactly one closer, in
    /// order, and nothing else starts a line with `'@`.
    #[test]
    fn the_here_strings_balance() {
        for config in [
            base(),
            Config {
                cloud_init: Some(CloudInit { manage_account: true }),
                ..base()
            },
            Config {
                generalize: false,
                computer_name: "win-server-01".into(),
                ..base()
            },
            // A hostile account name: quoted, and shaped like a terminator.
            Config { username: "'@x'@".into(), ..base() },
        ] {
            let script = install_block(&config);
            // Inside a here-string only a closer means anything; outside one,
            // a line ending `@'` opens one.
            let mut open = 0;
            let mut opened = 0;
            for line in script.lines() {
                if open == 1 {
                    if line.starts_with("'@") {
                        assert_eq!(line, "'@", "a closer with a tail");
                        open = 0;
                    }
                } else {
                    assert!(!line.starts_with("'@"), "a stray closer: {line}");
                    if line.ends_with("@'") {
                        open = 1;
                        opened += 1;
                    }
                }
            }
            assert_eq!(open, 0, "an unterminated here-string");
            // The task script always, the runner on a golden image.
            let want = if config.generalize { 2 } else { 1 };
            assert_eq!(opened, want, "{script}");
        }
    }

    /// The race: on the golden, OxideGeneralize and OxideCloudInit are both
    /// startup tasks and start together. OxideGeneralize writes the marker
    /// and then runs sysprep, so a marker check alone lets OxideCloudInit
    /// make a profile and start the service on the golden mid-sysprep. The
    /// task is therefore registered disabled on a golden build, and only the
    /// clone-side runner -- which only a clone's specialize pass runs --
    /// enables it.
    #[test]
    fn the_task_is_armed_only_on_a_clone() {
        let golden = install_block(&base());
        let disable = golden.find("Disable-ScheduledTask").expect("disabled");
        let register =
            golden.find("Register-ScheduledTask").expect("registered");
        assert!(register < disable);
        assert!(golden.contains("Enable-ScheduledTask"));
        assert!(golden.contains("oxide-generalized.txt"));

        // A named machine has no clone-side runner to enable it, so it is
        // registered enabled, and no runner is written.
        let named = install_block(&Config {
            generalize: false,
            computer_name: "win-server-01".into(),
            ..base()
        });
        assert!(named.contains("Register-ScheduledTask"));
        assert!(!named.contains("Disable-ScheduledTask"));
        assert!(!named.contains(CLONE_SCRIPT_PATH));
        assert!(!named.contains("oxide-generalized.txt"));
        assert!(!named.contains("sysprep"));
    }

    /// The marker `OxideGeneralize` writes is the marker `OxideCloudInit`
    /// tests. Each script is pulled out of the whole bootstrap and checked for
    /// the one path, so a rename in either module -- or a hand-typed spelling
    /// creeping back into one -- fails here rather than leaving every clone
    /// "not generalized yet" forever.
    #[test]
    fn both_tasks_name_the_same_generalize_marker() {
        let script = crate::bootstrap::build(&base())
            .expect("golden bootstrap")
            .replace("\r\n", "\n");
        let path = format!("\"$env:SystemDrive\\{GENERALIZED_MARKER}\"");
        let generalize = here_string(&script, "$gen");
        assert!(
            generalize.contains(&format!("$marker = {path}\n")),
            "{generalize}"
        );
        assert!(generalize.contains("Set-Content -LiteralPath $marker"));
        let task = here_string(&script, "$ciTask");
        assert!(
            task.contains(&format!("if (-not (Test-Path {path})) {{\n")),
            "{task}"
        );
        // Nothing spells it any other way.
        let spelled = script.matches("oxide-generalized").count();
        let named = script.matches(GENERALIZED_MARKER).count();
        assert_eq!(spelled, named, "{script}");
    }

    /// `C:\oxide` would inherit `C:\`'s ACL, which lets Authenticated Users
    /// modify what is created beneath it, and the clone-side runner in it is
    /// run as SYSTEM. So the block cuts inheritance, grants exactly three
    /// principals, and does it before anything is written inside -- guarded,
    /// because 'Stop' is in force.
    #[test]
    fn the_oxide_directory_is_not_user_writable() {
        for path in [LOG_DIR, CLONE_SCRIPT_PATH] {
            assert!(path.starts_with(&format!("{OXIDE_DIR}\\")), "{path}");
        }
        for config in [
            base(),
            Config {
                generalize: false,
                computer_name: "win-server-01".into(),
                ..base()
            },
        ] {
            let script = install_block(&config);
            let acl = oxide_dir_acl();
            assert!(script.starts_with(&acl), "{script}");
            assert!(acl.contains(&format!("$oxDir = '{OXIDE_DIR}'")));
            assert!(
                acl.contains("$oxAcl.SetAccessRuleProtection($true, $false)")
            );
            assert!(acl.contains(
                "$oxAcl.Access | ForEach-Object { $oxAcl.RemoveAccessRule($_)"
            ));
            for grant in [
                r"@('S-1-5-18', 'FullControl')",
                r"@('S-1-5-32-544', 'FullControl')",
                r"@('S-1-5-32-545', 'ReadAndExecute')",
            ] {
                assert!(acl.contains(grant), "{grant}");
            }
            assert_eq!(acl.matches("AddAccessRule").count(), 1);
            assert_eq!(acl.matches("@('").count(), 3, "exactly three grants");
            assert!(acl.contains(
                "New-Object System.Security.Principal.SecurityIdentifier"
            ));
            assert!(
                !acl.contains(r"BUILTIN\") && !acl.contains(r"NT AUTHORITY\"),
                "names are localized; every principal must be a SID"
            );
            assert!(acl.contains("'ContainerInherit,ObjectInherit', 'None'"));
            assert!(
                acl.contains("Set-Acl -LiteralPath $oxDir -AclObject $oxAcl")
            );
            assert!(acl.starts_with("# ") && acl.contains("\ntry {\n"));
            assert!(acl.ends_with(
                "} catch { Log \"WARNING: could not restrict access to \
                 ${oxDir}: $_\" }\n"
            ));
            // Before the runner or the log directory is created inside it.
            let set = script.find("Set-Acl -LiteralPath $oxDir").unwrap();
            assert!(set < script.find(LOG_DIR).unwrap());
            if config.generalize {
                assert!(set < script.find(CLONE_SCRIPT_PATH).unwrap());
            }
        }
    }

    /// Decision 3: the task never removes itself. Generalize gives every clone
    /// a new SID, so the profile has to be made again on every clone.
    #[test]
    fn the_task_never_unregisters_itself() {
        for manage_account in [false, true] {
            let script = install_block(&Config {
                cloud_init: Some(CloudInit { manage_account }),
                ..base()
            });
            assert!(!script.contains("Unregister-ScheduledTask"), "{script}");
        }
    }

    /// Every branch. The slug names the golden, the label is prose for a
    /// failure message -- kept separate so a reworded label cannot orphan a
    /// golden, exactly as in `unattend` and `bootstrap`.
    fn cases() -> Vec<(&'static str, &'static str, Config)> {
        let named = |c: Config| Config {
            computer_name: "win-server-01".into(),
            generalize: false,
            ..c
        };
        let manage = |c: Config| Config {
            cloud_init: Some(CloudInit { manage_account: true }),
            ..c
        };
        vec![
            ("golden-keep", "golden image, keep my account", base()),
            (
                "golden-manage",
                "golden image, cloud-init manages the account",
                manage(base()),
            ),
            ("named-keep", "named machine, keep my account", named(base())),
            (
                "named-manage",
                "named machine, cloud-init manages the account",
                manage(named(base())),
            ),
            (
                "golden-keep-no-rdp",
                "golden image, keep my account, no rdp",
                Config { enable_rdp: false, ..base() },
            ),
            (
                "named-manage-no-rdp",
                "named machine, managed account, no rdp",
                Config { enable_rdp: false, ..manage(named(base())) },
            ),
        ]
    }

    /// Rewrite the goldens from the current generator. The diff is the review.
    ///
    ///   cargo test -p oxwin-core dump_goldens -- --ignored
    #[test]
    #[ignore = "rewrites goldens; run deliberately and read the diff"]
    fn dump_goldens() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/cloudbase");
        std::fs::create_dir_all(&dir).expect("create testdata/cloudbase");
        for (slug, _, config) in cases() {
            std::fs::write(
                dir.join(format!("{slug}-service.conf")),
                service_conf(&config),
            )
            .expect("write golden");
            std::fs::write(
                dir.join(format!("{slug}-unattend.conf")),
                unattend_conf(&config),
            )
            .expect("write golden");
        }
    }

    #[test]
    fn matches_the_goldens() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/cloudbase");
        for (slug, label, config) in &cases() {
            for (suffix, ours) in [
                ("service", service_conf(config)),
                ("unattend", unattend_conf(config)),
            ] {
                let path = dir.join(format!("{slug}-{suffix}.conf"));
                // A missing golden fails. A test that skips is a test that lies.
                let theirs =
                    std::fs::read_to_string(&path).unwrap_or_else(|e| {
                        panic!(
                            "{}: {e}. Regenerate with dump_goldens",
                            path.display()
                        )
                    });
                assert_eq!(theirs, ours, "{suffix} conf differs for {label}");
            }
        }
    }

    /// A golden nothing compares against is dead weight, and a case with no
    /// golden would be caught only by the read above.
    #[test]
    fn every_golden_has_a_case_and_every_case_a_golden() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/cloudbase");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("testdata/cloudbase")
            .map(|e| {
                e.expect("entry").file_name().to_string_lossy().to_string()
            })
            .collect();
        on_disk.sort();
        let mut expected: Vec<String> = cases()
            .iter()
            .flat_map(|(slug, ..)| {
                [
                    format!("{slug}-service.conf"),
                    format!("{slug}-unattend.conf"),
                ]
            })
            .collect();
        expected.sort();
        assert_eq!(on_disk, expected);
    }
}
