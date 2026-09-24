# Cloud-init in the guest, and arbitrary files on the media

2026-09-22

## What this is

Two features, kept deliberately separate, both wanted for v1:

1. **Cloud-init** — upstream cloudbase-init, embedded in the payload, installed
   offline during the install, and run once per clone so each instance gets its
   own hostname, its own SSH keys, a full-size `C:`, and whatever its
   `user_data` says. On by default.
2. **Extra files** — any files the user picks ride the installer volume and are
   copied to `C:\oxide\extras` in the guest. No execution.

Cloud-init is not implemented *as* an extra file. The second feature is the
missing half of the supplied-answer-file escape hatch shipped on 2026-09-21: an
answer file can reference `E:\whatever` all it likes, but until now there was no
way to get a file onto the media, and the guest is air-gapped — no Windows
Update, no Feature-on-Demand source, no internet. Execution deliberately stays
with cloud-init's `user-data`, which runs per instance rather than being baked
into an image.

## Why upstream, not the fork Oxide's own docs name

`docs.oxide.computer/guides/working-with-windows-vms` points Windows users at
`https://oxide-omicron-build.s3.amazonaws.com/CloudbaseInitSetup.msi`: an
unversioned URL, 55 MiB, last modified **October 2022**, built from
`luqmana/cloudbase-init@oxide`. That fork carries three commits over upstream —
a dependency revert, "Also accept drives with vFAT label 'cidata' as a
configdrive", and "Support alternate 'public-keys' format for NoCloud service".

Both functional patches are in upstream master, read on 2026-09-22:

- `NoCloudConfigDriveService.__init__` passes `'cidata'` as the volume label
  (`cloudbaseinit/metadata/services/nocloudservice.py`).
- `get_public_keys` returns `raw_ssh_keys` directly when it is a list, which is
  the alternate format the patch added.
- `baseconfigdrive` honours `types=vfat` with `location=hdd`, which is how an
  Oxide config drive presents.

So the fork is obsolete, and upstream fits this repository's payload rules in a
way the S3 blob never could:

| | Oxide S3 MSI | Upstream stable |
|---|---|---|
| URL | unversioned | `cloudbase.it/downloads/CloudbaseInitSetup_1_1_8_x64.msi` (`_Stable_` 301-redirects to it) |
| Built | Oct 2022 | Apr 2026 |
| Size | 57 696 256 B | 64 843 776 B |
| SHA-256 | not published, not pinned | `0e7fa42e0cbc0ce7657f85730b0c6cc7afc4087a3639df0ff51a721a0be19bd5` |
| Signing | unverified | EV code-signed, `Cloudbase Solutions Srl`, SSL.com EV Code Signing |
| mtools | conf pointed at the install dir | bundled (`mcopy.exe`, `mdir.exe`); the installer sets `mtools_path` |

**This is source reading, not a booted guest.** "The patches are upstream" is
evidence that upstream *should* read an Oxide config drive; it is not evidence
that it does. A rack test is a gate on this work, not a follow-up. If a rack
shows the drive is not read, the fallback is documented rather than designed
away: `OXWIN_ASSETS=<dir>` already substitutes a payload directory, so dropping
the Oxide MSI in under the same filename gets it onto the media. **That alone is
not enough, and it is not "no code":** the bootstrap refuses to install any MSI
whose Authenticode signature is not valid and from `O=Cloudbase Solutions`, and
the Oxide fork's MSI is unsigned, so the guest logs `REFUSING to install
cloud-init` and installs nothing. Using the fork would need a deliberate change
to that check (or a fork build signed by someone the check is taught to trust),
which is a decision about what the guest will run, not a payload swap.

## Settings

```rust
pub struct Settings {
    // …
    /// `None` turns cloud-init off entirely: no MSI, no conf files, no
    /// sshd_config edit, and an image byte-identical to one built before this
    /// feature existed.
    pub cloud_init: Option<CloudInit>,
    pub extras: Vec<Extra>,
}

pub struct CloudInit {
    /// Mode B. See "The two account modes".
    pub manage_account: bool,
}

pub struct Extra {
    pub source: PathBuf,
    /// Where it lands on the volume, always under `/extras/`.
    pub volume_path: String,
}
```

`cloud_init` defaults to `Some(CloudInit { manage_account: false })` — on
everywhere, in the safer of the two modes. `extras` defaults to empty.

`unattend::Config` gains the same two pieces of information, since `bootstrap`
and `build_sysprep` are both generated from `Config`.

## The two account modes

A password remains mandatory in both, and in both the answer file creates the
account. That is not decoration: the serial console is the one way into a guest
whose network never came up, and SAC authenticates with a password and knows
nothing about SSH keys. A mode where cloud-init is the only account creator
would mean a missing config drive produces a machine with no way in at all.

### The profile trap (why the modes differ structurally)

`SetUserSSHPublicKeysPlugin` writes to `<user home>\.ssh\authorized_keys`, and
`WindowsUtils.get_user_home` resolves the home from
`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\<SID>` →
`ProfileImagePath`. That key exists only once a **profile** exists, and an
account created by an answer file has no profile until someone logs on. Without
one the plugin raises `User profile not found!` and the keys are simply absent.

The only thing in cloudbase-init that creates the profile is
`CreateUserPlugin.post_create_user`, whose own comment says so: *"Create a user
profile in order for other plugins to access the user home, etc."* And
`BaseCreateUserPlugin::execute` calls it on every path — including the
`elif osutils.user_exists(user_name)` branch, which **resets the account's
password to a fresh random one**.

So "reset my password" and "make the profile" are the same action upstream.
Hence:

**Mode A — keep my account** (`manage_account: false`, default). No
`CreateUserPlugin`, so nothing touches the password — and nothing makes the
profile either, so we do. The clone-side startup task that already exists for
`OxideGeneralize` gains a sibling, `OxideCloudInit`, which on first boot after
OOBE materialises the profile with `Start-Process -Credential -LoadUserProfile`
using the baked password, then starts the cloudbase-init service. Ordering is
explicit and every step is logged, rather than being a race between a service
and an OOBE that may not have finished.

**Mode B — cloud-init manages the account** (`manage_account: true`).
`CreateUserPlugin` is included; it finds the account the answer file made, sets
a per-instance random password, creates the profile, and the key plugin works.
The baked password is good from install until that runs, and the GUI and CLI
both say exactly that rather than leaving the user to discover it.

### sshd: both key sources have to work

Two key sources exist and they are not the same thing:

- **Baked keys** — typed into the tool, identical on every clone, written by
  `bootstrap.ps1` to `%ProgramData%\ssh\administrators_authorized_keys`.
- **Metadata keys** — attached to each Oxide instance at create time, delivered
  on the `cidata` drive, written by cloud-init to the user's
  `.ssh\authorized_keys`.

Stock Windows `sshd_config` ends with a `Match Group administrators` block that
redirects `AuthorizedKeysFile` to `administrators_authorized_keys` only, so an
administrator's per-user file is ignored: metadata keys silently do nothing. The
prototype's fix was to comment out both directives — which breaks the *baked*
keys, trading one silent failure for another.

The fix here is to rewrite the directive inside that block to name both files:

```
AuthorizedKeysFile .ssh/authorized_keys __PROGRAMDATA__/ssh/administrators_authorized_keys
```

`AuthorizedKeysFile` takes multiple paths. A test asserts the generated edit
contains both, and the rack checklist below verifies both files on a clone.

## What the builder writes

`builder::assemble` gains entries in the file list it already sorts by volume
path — that order decides cluster allocation and therefore the bytes of the
volume, so nothing here may depend on `readdir` order.

| Volume path | When | Contents |
|---|---|---|
| `\cloudbase\CloudbaseInitSetup_x64.msi` | cloud-init on | the embedded payload |
| `\cloudbase\cloudbase-init.conf` | cloud-init on | generated, goldened |
| `\cloudbase\cloudbase-init-unattend.conf` | cloud-init on | generated, goldened |
| `\extras\…` | `extras` non-empty | user files, relative structure preserved |

`config`, never `request.config`, decides anything release-dependent here.

### Payload

`tools/fetch-payload.sh` fetches the pinned, checksummed MSI to
`assets/cloudbase/CloudbaseInitSetup_x64.msi` and records it in
`payload-manifest.json` with its size, exactly like OpenSSH. `build.rs` embeds
it by walking `assets/`.

A missing MSI is **not** a build error, matching the existing rule: the table is
allowed to be empty so `cargo test` works on a fresh clone. The engine instead
refuses to build a cloud-init image and names `fetch-payload.sh`, and
`OXWIN_REQUIRE_PAYLOAD=1` makes CI's release builds fail rather than shipping a
binary whose cloud-init tickbox cannot work. `oxwin doctor` reports the MSI's
presence, version and fingerprint beside the driver payload.

The binary grows from about 26 MiB of payload to about 88 MiB. That is the cost
of a default-on feature in an air-gapped environment, and it buys the one-file
property the whole payload design exists for.

## What the guest does

### First pass — the install itself

`bootstrap.ps1`, as SYSTEM in `specialize`, gains, when cloud-init is on:

1. Verify the MSI's Authenticode signature — `Status -eq 'Valid'` and a subject
   matching `O=Cloudbase Solutions` — and refuse to install otherwise. The
   pinned SHA-256 proves we got the bytes we asked for; it says nothing about
   who signed them. Same fail-closed check the script already does for OpenSSH.
2. `msiexec /i … /qn /norestart RUN_SERVICE_AS_LOCAL_SYSTEM=1`, logging the exit
   code.
3. Copy both conf files into the install's `conf\` directory and delete the
   `Unattend.xml` the installer drops there.
4. `Set-Service cloudbase-init -StartupType Disabled`, so it cannot contend with
   the install's own passes. It is re-enabled per clone.
5. Rewrite `sshd_config`'s `AuthorizedKeysFile` as above.
6. Register `OxideCloudInit` (Mode A only).

And, independently of cloud-init, when `extras` is non-empty: copy `\extras` to
`C:\oxide\extras`, logging each file copied — so `C:\oxide-bootstrap.log`
answers "did my file make it?" without getting into the guest.

### Second pass — each clone's first boot

`unattend::build_sysprep`'s `specialize` pass gains two
`RunSynchronousCommand`s: re-enable the service, then run cloud-init once
against the unattend conf.

Not the prototype's inline `cmd.exe` incantation — at ~230 characters it sits
uncomfortably close to the 259-character `<Path>` cap, over which Setup rejects
the entire answer file with an error naming only the pass. Instead a short call
to a generated `C:\oxide\cloud-init.ps1`, which also lets us log the exit code
the way the sysprep work taught us to.

The prototype's `&& exit 1 || exit 2` convention is ported deliberately, with a
comment: `WillReboot=OnRequest` reads an exit code of 1 as "reboot requested",
and a successful cloud-init run that changed the hostname needs exactly that.
Success therefore maps to 1 and failure to 2, which is the opposite of every
other exit code in this codebase and will look like a bug to the next reader.

### Hostname: `*` and `SetHostNamePlugin` both fire

Our sysprep answer file keeps `<ComputerName>*</ComputerName>` in `specialize`,
and cloud-init sets the name from metadata in the same pass. This is
belt-and-braces and is load-bearing in both directions: `*` guarantees a valid
unique name if the config drive is ever missing, and cloud-init overwrites it
with the instance's name when it is there. The prototype emitted no
`ComputerName` at all, which is the more fragile choice. A comment and a test
say so, rather than leaving it to be rediscovered.

For a **named** deployment `SetHostNamePlugin` is omitted entirely: the user
typed a name and it is not cloud-init's to overwrite.

## The conf files

Generated from `Config`, pinned by goldens in `testdata/cloudbase/`,
regenerated by `dump_goldens`.

`cloudbase-init-unattend.conf` — the one-shot `specialize` run:

```ini
[DEFAULT]
allow_reboot=false
stop_service_on_exit=false
check_latest_version=false
netbios_host_name_compatibility=false
ntp_enable_service=true
real_time_clock_utc=true
rdp_set_keepalive=true          # only when RDP is enabled
verbose=true
debug=true
log_dir=C:\oxide\log
log_file=cloudbase-init-unattend.log
metadata_services=cloudbaseinit.metadata.services.nocloudservice.NoCloudConfigDriveService
plugins=…NTPClientPlugin, …SetHostNamePlugin, …ExtendVolumesPlugin, …RDPSettingsPlugin
[config_drive]
types=vfat
location=hdd
raw_hdd=false
cdrom=false
vfat=false
```

`SetHostNamePlugin` only for a golden image; `RDPSettingsPlugin` only when RDP
is enabled.

`cloudbase-init.conf` — the service, every boot: `username` and
`groups=Administrators` from the credentials, the same single metadata service
and config-drive block, `log_file=cloudbase-init.log`, and plugins
`CreateUser` (Mode B only), `SetHostName` (golden only),
`SetUserSSHPublicKeys`, `ExtendVolumes`, `UserData`, with `user_data_plugins`
set to the cloud-config and shell-script plugins so both `#cloud-config` YAML
and `<powershell>`/`<script>` blobs work.

### Four deliberate departures from the prototype

1. **`metadata_services` names exactly one service.** The stock list probes HTTP
   metadata endpoints that do not exist on a rack; on a guest with no route that
   is a boot spent waiting for timeouts.
2. **`WindowsAutoUpdatesPlugin` is gone.** The prototype set
   `enable_automatic_updates=true`. This tool has no update policy setting, and
   turning automatic updates on across someone's fleet is a policy decision
   disguised as a default. Windows' own default stands; a real setting can add
   it later.
3. **No serial logging.** The prototype wrote to `COM3`. An Oxide instance
   surfaces `COM1`, which carries SAC and EMS and where debug chatter would be
   actively harmful, and nothing establishes that `COM3` is reachable at all —
   so that line was likely writing into the void. Files only.
4. **`log_dir=C:\oxide\log`.** Everything this tool leaves in a guest is then in
   one place, beside `C:\oxide-bootstrap.log` and `C:\oxide\extras`, which
   matters on a machine reachable only over a serial console.

Also omitted, each for a reason: Heat, the WinRM listener and certificate
plugins, `SetUserPasswordPlugin` (Oxide metadata carries no admin password), and
the v2 network plugins (Oxide gives DHCP; static configuration would fight it).

## Extra files

The user picks files or folders; relative structure is preserved under
`\extras` on the installer volume and copied to `C:\oxide\extras`. Refused:
absolute paths and `..` components in the volume path, and a total size that
would not fit the volume. No execution, no ordering, no fixed hook name — the
`user-data` path already covers running something, per instance rather than per
image, and arbitrary user code in the `specialize` pass is exactly the shape of
hang that this project keeps paying for. If a hook is wanted later it is
additive, with one obvious name.

## Surfaces

- **CLI**: `--no-cloud-init`, `--cloud-init-account=keep|manage`, and a
  repeatable `--extra=<path>`. A new `cidata` subcommand (below) adds a row to
  `oxwin_cli::COMMANDS`, to `run`'s `match`, and to `dispatch::route`'s table —
  the existing test that reads the arms out of the source keeps the three in
  step.
- **GUI**: a cloud-init tickbox with a nested account-mode choice shown only
  when it is on, and a fixed-height extras list. A list that grows with its
  contents would move the primary button, which the layout rules forbid; the
  height comes from the widget, not the data.
- **`doctor`**: the embedded MSI's version and fingerprint.
- **Docs**: a README section; `THIRD-PARTY.md` and `licenses/` gain
  cloudbase-init's Apache-2.0 notice and the Python runtime the MSI bundles.

## Testing

### Local end-to-end, no rack

`fat32.rs` already writes a deterministic FAT32 volume, so a new
`oxwin cidata <out.img> --hostname … --key … --user-data <file>` subcommand
generates a real NoCloud drive labelled `cidata`, and `tools/qemu-test.sh
--cloud-init` attaches it as a second disk. That exercises the MSI install,
service ordering, profile creation, hostname, keys and `user-data` against
actual cloudbase-init.

What QEMU still cannot test, unchanged from today and now written down for
cloud-init too: the VPC firewall (RDP looks fine locally and is dropped on a
rack) and the chooser's second branch, because a QEMU harness honours guest
NVRAM.

Open implementation question, to settle by reading cloudbase-init's vfat
detection rather than guessing: whether the volume label comparison is
case-sensitive. FAT32 labels are conventionally uppercase, cloud-init documents
both `cidata` and `CIDATA`, and the code passes the lowercase spelling. The
`cidata` generator writes whichever spelling the code actually accepts, and a
test pins it.

### Goldens

Both conf files across mode A/B × golden/named × RDP on/off; `bootstrap.ps1`
with cloud-init on, off, and with extras; the sysprep answer file with its new
commands; the `sshd_config` rewrite. Every comparison mutation-tested before it
is believed. `builds_the_same_image_by_every_route` continues to cover
determinism, now with the MSI and extras in the file list.

### New lint rule

A supplied answer file plus cloud-init. `build_sysprep` is ours either way, so
the `specialize` run survives — but if the supplied file never invokes
`setup\bootstrap.ps1`, the MSI is never installed and every one of those
commands fails on a clone, silently. Same family as the existing warning, and it
belongs beside it.

### The rack checklist

A finished install proves nothing, so on a clone check the artefact:

- `C:\oxide\log\cloudbase-init.log` and `…-unattend.log` exist and name the
  config drive they found.
- The computer name equals the instance name. An `OXIDEOX-…` name means the
  drive was never read.
- `C:\Users\<user>\.ssh\authorized_keys` holds the **instance's** key, and
  `%ProgramData%\ssh\administrators_authorized_keys` still holds the **baked**
  one. Both must authenticate.
- `C:` is the full disk size, not the image's.
- `C:\oxide-bootstrap.log` shows the signature check passing, the MSI exit code,
  and the `sshd_config` edit.

`TESTED-MEDIA.md` records which releases this has been verified on. Server 2019,
Server 2022 and Windows 10 are the releases that can be verified at all; Server
2025 and Windows 11 cannot be hardware-tested while the Propolis NVMe problem
stands, so emulation is the only evidence available for them.

## Traps for CLAUDE.md

Each fails silently — the build succeeds and the install looks fine:

- **A cloud-init user with no profile gets no keys.** `get_user_home` reads
  `ProfileList\<SID>\ProfileImagePath`; an answer-file account has no profile
  until logon, and the plugin then raises `User profile not found!`. Only
  `CreateUserPlugin` makes one upstream, and it resets the password doing it.
- **`Match Group administrators` in `sshd_config` ignores per-user
  `authorized_keys`.** Metadata keys land in a file sshd will not read. Name
  both files in `AuthorizedKeysFile`; do not delete the block, which is where
  the baked keys live.
- **The stock `metadata_services` list probes the network.** On a rack those
  endpoints do not exist, and the guest spends its first boot timing out.
- **No MSI in the payload means cloud-init silently does nothing.** Hence the
  engine's refusal and `OXWIN_REQUIRE_PAYLOAD=1`.
- **Success is exit code 1** for the cloud-init `RunSynchronousCommand`, because
  `WillReboot=OnRequest` reads 1 as "reboot requested".

## Out of scope

- Static network configuration from metadata. Oxide gives DHCP.
- WinRM, Heat, and password-from-metadata plugins.
- An `extras\setup.ps1` execution hook.
- A cloud-init-only account with no password. `Credentials` stays a struct.
- Any change to `oxwin-rack`: the config drive is the control plane's, and
  nothing here needs the SDK.
