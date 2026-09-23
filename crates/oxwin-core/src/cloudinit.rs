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

/// Where both runs log. Created by the bootstrap, because nothing establishes
/// that cloudbase-init creates a missing `log_dir` itself.
pub const LOG_DIR: &str = r"C:\oxide\log";

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
/// is already the configured set, so it is left unset. This matches the
/// spec's names and values exactly -- no departure to record.
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
    ];
    if config.enable_rdp {
        lines.push("rdp_set_keepalive=true".into());
    }
    lines.extend([
        "verbose=true".into(),
        "debug=true".into(),
        format!("log_dir={LOG_DIR}"),
        "log_file=cloudbase-init-unattend.log".into(),
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
// Anything that has to be fixed at build time -- the account name, the
// password, whether this is a golden image -- is therefore spliced in here, by
// Rust, as literal text. Those do not nest, and a line starting `'@` inside
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
    s.push_str(
        r#"if (-not (Test-Path $exe)) { CLog "no cloudbase-init at $exe; nothing to run"; exit 0 }
$code = $null
try {
  $proc = Start-Process -FilePath $exe -ArgumentList "--config-file","`"$conf`"" -Wait -PassThru -ErrorAction Stop
  $code = $proc.ExitCode
} catch { CLog "could not start cloudbase-init: $_" }
CLog "cloudbase-init exited $code"
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
         # profile first. Registered in both account modes: a clone's specialize pass\n\
         # can set the service to Automatic, but the SCM has already started automatic\n\
         # services by then, so without this task the service would first run on the\n\
         # clone's second boot.\n"
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
# This task never unregisters itself and re-arms nothing: every step below is
# idempotent, so it simply runs at every startup. Generalizing gives each clone
# a new SID, so the profile it makes has to be made again on every clone -- an
# "unregister when done" edit would work on the golden image and silently break
# every machine cloned from it.
"#,
    );
    if golden {
        s.push_str(
            r#"# A golden image: before it is generalized there is nothing to do, and it
# shuts down straight after. oxide-generalized.txt is written just before the
# generalize and captured into the image, so it is there only on a clone.
if (-not (Test-Path "$env:SystemDrive\oxide-generalized.txt")) {
  TLog "not generalized yet; nothing to do until this image is cloned"
  exit 0
}
"#,
        );
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
  $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
"#,
    );
    s.push_str(&format!(
        "  Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action \
         -Trigger $trigger -Principal $principal -Force | Out-Null\n"
    ));
    if golden {
        s.push_str(&format!(
            "  # Disabled on a golden image. OxideGeneralize is a startup task too and\n\
             \x20 # starts alongside this one; it writes oxide-generalized.txt and then runs\n\
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
    // The password is already cleartext in autounattend.xml and in the sysprep
    // answer file, so writing it here is no new class of exposure. It is left
    // out when it is not printable ASCII: this script is written with
    // -Encoding ASCII, which would turn anything else into '?', and a line
    // break could put `'@` at the start of a line and end the here-string
    // this sits in. CreateProfile below needs no password.
    if config.password.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        s.push_str(&format!("$password = {}\n", ps_quote(&config.password)));
    } else {
        s.push_str(
            "# The password is not printable ASCII, so it is not written here.\n\
             $password = $null\n",
        );
    }
    s.push_str(
        r#"$sid = $null
try {
  $sid = (New-Object System.Security.Principal.NTAccount($user)).Translate([System.Security.Principal.SecurityIdentifier]).Value
} catch { TLog "WARNING: no account named $user to make a profile for: $_" }
if ($sid) {
  $profileKey = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$sid"
  $profilePath = (Get-ItemProperty -Path $profileKey -ErrorAction SilentlyContinue).ProfileImagePath
  if ($profilePath) {
    TLog "profile for $user already exists at $profilePath"
  } else {
    TLog "no profile for $user; making one so cloud-init can write its SSH keys"
    if ($password) {
      try {
        $cred = New-Object System.Management.Automation.PSCredential($user, (ConvertTo-SecureString $password -AsPlainText -Force))
        Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -ArgumentList '-NoProfile','-Command','exit' -Credential $cred -LoadUserProfile -Wait -ErrorAction Stop
      } catch { TLog "logging on as $user to load its profile failed: $_" }
      $profilePath = (Get-ItemProperty -Path $profileKey -ErrorAction SilentlyContinue).ProfileImagePath
    }
    # Fallback. Start-Process -Credential is CreateProcessWithLogonW, which is
    # documented not to work from LocalSystem -- and this task is LocalSystem.
    # userenv's CreateProfile makes the same ProfileList entry and profile
    # directory, and needs no password.
    if (-not $profilePath) {
      try {
        Add-Type -Namespace Oxide -Name UserEnv -MemberDefinition '[DllImport("userenv.dll", CharSet = CharSet.Unicode)] public static extern int CreateProfile(string sid, string name, System.Text.StringBuilder path, uint len);' -ErrorAction Stop
        $buf = New-Object System.Text.StringBuilder 260
        $hr = [Oxide.UserEnv]::CreateProfile($sid, $user, $buf, 260)
        TLog ('CreateProfile returned 0x{0:X8}' -f $hr)
      } catch { TLog "CreateProfile failed: $_" }
      $profilePath = (Get-ItemProperty -Path $profileKey -ErrorAction SilentlyContinue).ProfileImagePath
    }
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

/// The `AuthorizedKeysFile` rewrite. Emitted inside `bootstrap`'s
/// `enable_ssh` branch, inside its `if (Get-Service sshd)` block, and only when
/// cloud-init is on: with it off there are no per-user keys to read.
pub fn sshd_fix_block() -> String {
    r#"  # Stock sshd_config ends with `Match Group administrators`, which points
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
      $new = [regex]::Replace($text, '(?m)^[ \t]*#?[ \t]*AuthorizedKeysFile[ \t]+__PROGRAMDATA__/ssh/administrators_authorized_keys[ \t]*$', $want)
      if ($new -ne $text) {
        Set-Content -LiteralPath $sshdConf -Value $new -Encoding ascii
        Log "sshd_config: AuthorizedKeysFile now names both key files"
        if (Get-Service -Name sshd -ErrorAction SilentlyContinue) { Restart-Service sshd -ErrorAction SilentlyContinue }
      } else {
        Log "WARNING: sshd_config has no administrators_authorized_keys directive to rewrite; per-instance keys may be ignored"
      }
    } else {
      Log "WARNING: no sshd_config; per-instance keys will not be read"
    }
  } catch { Log "WARNING: could not rewrite sshd_config; per-instance keys may be ignored: $_" }"#
    .to_string()
}

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
        assert!(keep.contains("-LoadUserProfile"));
        let managed = install_block(&Config {
            cloud_init: Some(CloudInit { manage_account: true }),
            ..base()
        });
        assert!(!managed.contains("-LoadUserProfile"));
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
        // The block itself stays: it is where the baked keys live.
        assert!(!block.contains("Match Group administrators\n#"));
        // And a missing file is a logged warning, never a throw: the script
        // runs under $ErrorActionPreference = 'Stop'.
        assert!(block.contains("Test-Path"));
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

    /// Mode A's profile step needs the password, so it is in the task script.
    /// It is already cleartext in autounattend.xml and the sysprep answer file,
    /// so this is no new exposure -- but it must survive a single quote, or
    /// the task script does not parse and no clone ever starts cloud-init.
    #[test]
    fn a_password_or_name_containing_a_quote_is_escaped() {
        let script = install_block(&Config {
            username: "o'brien".into(),
            password: "it's-a-secret".into(),
            ..base()
        });
        assert!(script.contains("$password = 'it''s-a-secret'"), "{script}");
        assert!(script.contains("$user = 'o''brien'"), "{script}");
        assert!(!script.contains("'it's"));
    }

    /// The task script is written with -Encoding ASCII, so a password that is
    /// not printable ASCII would arrive as '?', and a line break in one could
    /// end the here-string it sits in. It is left out; CreateProfile needs no
    /// password.
    #[test]
    fn a_password_that_is_not_printable_ascii_is_left_out() {
        for password in ["p\u{e4}sswort-long", "line\n'@break"] {
            let script =
                install_block(&Config { password: password.into(), ..base() });
            assert!(script.contains("$password = $null"), "{script}");
            assert!(!script.contains(password));
            assert!(script.is_ascii());
            assert!(script.contains("CreateProfile"));
        }
    }

    /// Mode B never needs the password, so it is not written.
    #[test]
    fn mode_b_does_not_write_the_password() {
        let managed = install_block(&Config {
            cloud_init: Some(CloudInit { manage_account: true }),
            ..base()
        });
        assert!(!managed.contains(&base().password), "{managed}");
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
            // A hostile password: quoted, and shaped like a terminator.
            Config { password: "'@x'@".into(), ..base() },
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
