# Windows Image Builder — working notes

Builds bootable Windows install media for an Oxide rack: a single disk image that
installs Windows unattended, with no console interaction. An eframe/egui desktop app
plus a CLI that share one engine.

See `README.md` for the user-facing guide, `DEVELOPMENT.md` for architecture, and
`PLAN.md` for what is coming. `v1/` is gone — the previous `wimsy` builder lives on in
this repository's history on `main`, not in the tree.

## Commands

```
cargo run --release -p oxwin-gui           # the app
cargo run -p oxwin-gui -- path/to.iso      # opened on a file
cargo run -p oxwin-cli                     # doctor: assets, oxide CLI
cargo run --release -p oxwin-cli -- build <iso-or-mount> out.img \
  --password=… --name=…                    # the engine, from a script
cargo test                                 # 183 tests, no rack or ISO needed
OXWIN_TEST_ISO=~/Desktop/…iso cargo test   # adds the real-media UDF test
OXWIN_TEST_ESP=p2.bin OXWIN_TEST_ASSETS=assets/efi cargo test
                                           # adds the full 64 MiB FAT32 compare
cargo clippy --all-targets && cargo fmt
```

`./tools/fetch-payload.sh` downloads the third-party payload into `assets/`. Nothing
works without it having been run once.

## The payload is compiled in

`build.rs` walks `assets/` and embeds every file with `include_bytes!`, so a release
binary is one file with nothing to locate at runtime. A downloaded binary has no
repository to walk up into, which is what the old discovery assumed.

- **A missing `assets/` is not a build error.** It embeds an empty table, so `cargo test`
  works on a fresh clone. The engine then refuses to build with a message naming
  `fetch-payload.sh`, rather than producing an image with no drivers.
- **`OXWIN_REQUIRE_PAYLOAD=1` makes it a hard failure.** CI sets this for release builds,
  so an empty table cannot ship as though it were real.
- **What is embedded is cross-checked against `payload-manifest.json`**, at build time
  from the directory and again at test time from the table that shipped. Provenance is
  enforced earlier, by the SHA-256 checksums in `fetch-payload.sh`.
- **`OXWIN_ASSETS=<dir>`** (or `--assets=<dir>`) substitutes a directory, which is how a
  driver version gets tried without a rebuild.
- `oxwin doctor` prints a fingerprint of the embedded payload, so two artifacts built on
  different platforms can be shown to carry the same bytes.

## Ground rules

- **Third-party binaries are never committed.** `tools/fetch-payload.sh` downloads them
  against pinned versions and SHA-256 checksums into `assets/`, which is gitignored. CI
  runs that script.
- **A finished install proves nothing on its own.** Three consecutive "successful"
  installs each hid a defect. Check the artefact: `C:\oxide-bootstrap.log` exists, the
  NIC is bound, the intended edition installed.
- **The tests do not build an image.** Everything that needs real media is behind an
  unset environment variable, so a green suite is consistent with the image being
  broken. The real-ISO build is a separate, manual gate.

## Style

Matched to omicron so this reads like the rest of Oxide's Rust: `max_width = 80`,
`use_small_heuristics = "max"`, edition 2024, toolchain pinned to 1.97.1, and the
`disallowed_*` clippy lints warned while clippy's style group is allowed. Do not change
these to taste — the point is that they are not ours.

## The media is detected, never asserted

`media::inspect` reads `ARCH`, `PRODUCTTYPE`, `BUILD` and `INSTALLATIONTYPE` out of the
WIM XML resource and returns a `MediaInfo`. `builder::assemble` calls
`media::problems_for` on the image list it has already read, refuses blocking problems,
and **overwrites `config.release` with the detected release**. That is the choke point:
the GUI reaches it through the engine and the CLI reaches it directly, so a refusal there
is a refusal everywhere.

- `settings.release` is a starting point, not an input. A radio button is an assertion
  nothing checks, and it is stale the moment someone picks a different ISO.
- `WindowsRelease::driver_dir` and `base_builds` are the one release table. `ALL` is every
  variant. Iterate `ALL`, never a literal list — `unattend`'s `every_release_has_a_golden`
  does exactly that, so a new release cannot arrive without a pinned answer file.
- An unfamiliar build still builds, as the nearest known release, with a warning naming
  the build and the driver directory guessed. Server 2016 should not be walled off by a
  table nobody updated.
- The GUI reads the media in `set_iso` — about 10 ms — so stage 2 offers the editions
  that ISO carries. `Draft.image_index` is the choice, and `Settings::edition` carries it
  as an index string, because an index is the only unambiguous selector: two images on
  server media share `ServerDatacenterEval` and differ only in Core-ness.
- `Experience` is gone. Core-ness is a property of the image picked, not a switch beside
  it, and `unattend` only needs an edition id when there is no image index.

## Architecture rule

**`oxwin-core` never prints and never blocks on a human.** It emits `progress::Event`
values and returns `Result`. That single constraint is what makes the CLI nearly free;
the moment a `println!` or a prompt appears in the core, the CLI becomes a rewrite.

`oxwin-rack` is where the Oxide SDK lives, deliberately not in the core: the SDK brings
tokio, reqwest and rustls, and the core's four dependencies and fast golden suite are both
worth keeping. It owns a current-thread runtime internally and exposes a **blocking** API
emitting the same `progress::Event`, so async never crosses the crate boundary. It
inherits the no-printing rule and adds **no TTY, anywhere** — no prompt, no picker,
nothing from stdin — because the CLI has to work over SSH and in CI. No token ever leaves
`oxwin_rack::profile`; callers get a `Selector`, and naming a profile when the user did not
choose one silently disables `OXIDE_TOKEN`.

The GUI holds a `Draft` of raw strings and bools and converts to a core `Settings` on
demand. Do not bind widgets directly to the core enums — a radio button that rebuilds
the enum throws away a half-typed hostname.

## The goldens are the gate

Every generated structure is pinned byte for byte by a committed golden: the answer file
(`testdata/unattend/`), the bootstrap script (`testdata/bootstrap/`), exFAT volumes
(`testdata/exfat/`), and the FAT32 ESP and media reference (`testdata/reference-*.txt`).

Those goldens were generated by the JavaScript builder this was ported from, whose
output is the only version that has installed Windows on real Oxide hardware. That
builder is not in this repository and does not need to be; the bytes carry its
provenance.

Rules that came out of building it:

- **Mutation-test every comparison before believing it.** A green diff nobody has tried
  to break is not evidence.
- **A test that skips is a test that lies.** The predecessors of these tests generated
  their reference by running the JavaScript and skipped when it was absent, which meant
  a machine without Node ran a suite that proved nothing while reporting success. A
  missing or orphaned golden now fails.
- **`dump_goldens` rewrites from the current code, so its diff is the review.** An
  unexplained hunk is a bug being blessed, not a test being updated.
- **Port the quirk, not the intent.** Where the original does something wrong that no
  input of ours reaches — truncating a dotted directory name — this does the same thing
  with a test saying why. Changing it would change every volume's bytes, including
  images already booted.
- **Assume any remaining nondeterminism is a bug.** Three have been found: local-time
  getters in both filesystem builders, and `readdir` ordering of the media file list.
  `builds_the_same_image_by_every_route` is the guard.

## egui 0.36 differs sharply from older releases

Ignore examples found online; they are almost all pre-0.36.

- `eframe::App` implements **`fn ui(&mut self, ui: &mut Ui, frame: &mut Frame)`**, not
  `update(&mut self, ctx, frame)`. The app receives a `Ui`, not a `Context`.
- Panels take `&mut Ui`: `egui::Panel::top(id).exact_size(h).show(ui, …)`.
  `TopBottomPanel` and `SidePanel` are gone, replaced by a unified `Panel`.
- Styles are per-theme: `ctx.all_styles_mut(|s| …)`, not `ctx.set_style`.
- `DroppedFile` is a trait behind an `Arc`; `f.path()` is a method returning `&Path`.
- `Painter::rect` takes a `StrokeKind`; `Margin::same` takes an `i8`.

## Design rules

- **Green means state or action**, never a label. A stage is done, a control is on, this
  is the button to press. Section headings are dim uppercase. Spending the accent on
  four static headings per screen drains it of meaning.
- **`theme::CORNER` is 3px** and applies to every widget. Oxide's house style is
  square-ish; heavily rounded panels read as consumer software.
- **Never type a glyph that might not exist in the default font.** `→` shipped as a
  missing-glyph box next to a button. The stepper's check mark is painted from two line
  segments for the same reason.
- **Let the layout engine reserve space.** Action rows go in `egui::Panel::bottom(…)`
  declared *before* the scroll area, which then gets exactly what is left.
  Hand-computing the reservation was wrong three times running, because the enabled
  primary button is taller than the disabled one.
- **No disabled button on an empty stage**, and when a button is disabled, say what is
  actually wrong next to it rather than "resolve the items above" — the reason may be
  scrolled out of sight.
- Scroll areas use `auto_shrink([false, false])`, or they size to their content and
  float inside the window instead of filling it.
- **A control sized by the data needs a constant-height widget.** The edition picker is
  as long as the media says: four rows on server media, eleven on the Windows 11 22H2
  retail ISO, one on the Windows 10 evaluation. As radio buttons that pushed the password
  field off the first screen; as a `ComboBox` it is one row whatever the media. One
  option is furniture — show a label naming it instead.

## Credentials: a password is mandatory

`Credentials` is a struct, not an enum, because key-only was an invalid state. SAC
(serial) and RDP both authenticate with a password and know nothing about SSH keys, so a
key-only account is reachable over SSH and nowhere else — including from the serial
console, which is the one way in when something has gone wrong. Keys are additive.

There is no default password anywhere in this workspace; `Settings::default()` is
deliberately unbuildable.

## Read what the machine wrote down before reasoning about what Windows does

Three wrong diagnoses in a row on the golden-image work shared one shape: reasoning from
documented behaviour instead of reading the guest's own logs. `fDenyTSConnections` was
already `0` when RDP was "broken"; the local account was created without complaint when
the theory said it would fail; sysprep returns on success when the comment said it could
not. Each was settled in seconds by a log file.

- **`C:\Windows\Panther\UnattendGC\setupact.log`** is the generalize/specialize/OOBE
  log. It names the pass, the component, the generated computer name, and — decisively —
  which OOBE wizard page went active.
- **`C:\Windows\Panther\setupact.log` / `setuperr.log`** cover the original install.
- **`C:\oxide-bootstrap.log`** is ours, written to the system drive *and* the installer
  media, so it can be read by attaching that disk to a machine that works.
- **A guest with no framebuffer cannot show you an OOBE screen.** That is why these logs
  matter more here than on a normal machine, and why `tools/qemu-test.sh --vnc` exists as
  the fallback when a log is not enough.

## Traps that have already cost days

Each of these fails **silently** — the build succeeds and the install looks fine.

- **A generalize cycle never runs `windowsPE`, so nothing sets the locale.** On a normal
  install `Microsoft-Windows-International-Core-WinPE` satisfies OOBE's Localization page
  and it never appears. After `sysprep /generalize` that pass does not run, the page
  becomes active, and OOBE stops there waiting for a click — forever, on a guest with no
  framebuffer. `unattend::build_sysprep` therefore adds `Microsoft-Windows-International-Core`
  to its `oobeSystem` pass. The tell is one line in `C:\Windows\Panther\UnattendGC\setupact.log`:
  a completing install logs `SETACTIVE: wizard page for page End`, a stuck one logs
  `SETACTIVE: wizard page for page Localization`.
- **`sysprep /shutdown` returns immediately; it does not block until power-off.** Code
  after the call runs on *success*, not only on failure. Assuming otherwise put the
  recovery path — remove the "already generalized" marker, re-register the task — on the
  success path, so every boot generalized the machine and shut it down again. Branch on
  `$LASTEXITCODE`, never on control flow having reached a line after a command that
  "ends" the machine.
- **Windows deletes the password from the answer file it caches.** After Setup processes
  `autounattend.xml` it writes a copy to `C:\Windows\Panther\unattend.xml` with every
  `<Password>` replaced by the literal string `*SENSITIVE*DATA*DELETED*`. A
  `sysprep /generalize` with no `/unattend:` falls back to precisely that file, so
  `oobeSystem` cannot create the local account and the machine stops at the out-of-box
  wizard — forever, on a guest with no console. Seen on a rack: `OOBEInProgress=1`, a
  valid IP, every driver bound, and no reachable RDP. **RDP was not the problem**;
  `fDenyTSConnections` was already `0` and the listener simply does not accept
  connections until OOBE finishes. A golden build therefore writes its own copy, with the
  password intact, and names it: `unattend::build_sysprep` emits `specialize` and
  `oobeSystem` only — never `windowsPE`, which carries `DiskConfiguration`.
- **`<Path>` is capped at 259 characters** by the unattend schema. Over that, Setup
  rejects the whole answer file with an error naming only the pass, which reads like
  malformed XML.
- **An empty `<ProductKey><Key></Key>` is not the same as omitting the element.** Empty
  means "install with no key" and stalls at the EULA. Evaluation media needs the element
  gone entirely.
- **`PnpCustomizationsNonWinPE` is only valid in `offlineServicing`/`auditSystem`.** In
  `specialize` it is ignored with no error and NetKVM never installs, so the guest comes
  up with no network.
- **One constant in two places drifted** (`VOLUME_LABEL`: `OXIDE_UA` vs `WINSETUP`) and
  the bootstrap script was skipped on *every* install while installs still appeared to
  succeed. The locator now scans filesystem drives instead of trusting a label.
- **`request.config` and `config` are two different releases** in `builder::assemble`. The
  local `config` carries the release detected from the media; `request.config` carries
  what the caller asserted. Reading the wrong one gave the answer file the detected
  release and the drivers the asserted one, so Server 2025 media built with Server 2022
  drivers while every log line said 2025. Nothing after `config` is created may consult
  `request.config`. `builds_the_same_image_by_every_route` now builds once with a
  deliberately wrong asserted release and requires identical bytes.
- **Edition matching finds Server Core.** Media names only the Core images
  (`SERVERDATACENTERCORE`) and leaves Desktop Experience unmarked (`SERVERDATACENTER`),
  so a substring match on `datacenter` hits Core first. That shipped once. `selectImage`
  filters on Core-ness before matching, and a golden pins both spellings.
- **Driver payload filtering.** Filtering the virtio download by extension dropped
  `netkvmp.exe` and NetKVM then failed to install. The filter is now an exclusion
  (`! -iname '*.pdb'`).
- **The media file list must be sorted by the path the file will have on the volume.**
  That order decides which clusters each file gets, so it decides the bytes of the
  volume — and `readdir` order differs between a mounted ISO, an APFS copy and a Linux
  host. Nothing broke; no two builds simply agreed.
- **`sysprep.exe` does not set `$LASTEXITCODE`.** `& sysprep.exe …` followed by
  `$code = $LASTEXITCODE` leaves `$code` null, `$code -eq 0` is then False, and a sysprep
  that *succeeded* takes the failure path — which deletes the generalize marker and
  re-arms the task. The machine then generalizes and shuts itself down on every boot: a
  fleet that will not stay on. Found on a rack over three cycles, and the tell is one
  character wide: `sysprep FAILED with exit code ` with no number after it. Use
  `Start-Process -Wait -PassThru` and read `.ExitCode`. **And treat an unknown exit code
  as not-a-failure**, because the two errors are not symmetric — undoing the marker after
  a sysprep that worked is unrecoverable without a console, while leaving it after one
  that failed only costs a manual retry.
- **A clone that comes up has not passed.** The self-generalizing failure above appears
  about a minute *after* the machine reaches a normal startup, so a clone check that
  finishes at the first successful connection to port 22 passes on exactly the broken
  image it exists to catch. `Watch::for_clone` requires it to stay up for
  `CLONE_SETTLE`.
- **A SNAT external IP is outbound only.** Nothing can connect *to* it, so polling one
  for reachability never answers and a machine that installed perfectly reports as a
  failed install. `golden::steps::pick_address` takes Ephemeral or Floating and never
  SNAT.
- **A guest reboot leaves the instance `running`; a guest shutdown stops it.** That
  asymmetry is the entire completion signal for a golden build — Windows Setup reboots
  several times and the instance never leaves `running`. It is also why the watcher is
  not keyed on the serial console: desktop editions write nothing to COM1, so such a
  watcher works on server media and hangs forever on Windows 10 and 11.
- **Timestamps carry no clock and no time zone.** Both filesystem builders encode a
  fixed instant, held as the already-encoded field so a local-time conversion is
  unrepresentable. The originals used local-time getters, so every directory entry
  carried the builder host's zone and no two people produced the same image.

## Oxide-specific behaviour

- **Boot order belongs to the control plane, not the guest.** `boot_disk` is pinned at
  instance creation, there is no fallthrough to the next disk, and guest NVRAM is
  ignored. This is why the media carries a UEFI Shell chooser that detects an installed
  Windows and boots it — so the installer is safe to leave attached.
- **Disks must be a whole number of GiB** and at least 1 GiB. Without rounding up, the
  first real upload is rejected.
- **The disk must be imported with a 512-byte block size.** Not a preference: the MBR
  expresses partition offsets in sectors, so p1 at LBA 2048 means 1 MiB on a
  512-byte-block disk and 8 MiB on a 4096-byte one. Import the same bytes onto the wrong
  block size and the partition table points at nothing. Anything generating an
  `oxide disk import` command must pass `--disk-block-size 512` explicitly rather than
  trusting a default.
- **The default VPC allows only tcp/22 and ICMP.** Enabling RDP in the guest is
  necessary but not sufficient — 3389 is dropped before it reaches Windows. Adding the
  rule is a control-plane action and cannot come from the answer file.
- An instance needs `external_ips: [{"type": "ephemeral"}]` to be reachable at all.

## Windows media facts

- A Windows Server ISO is a UDF-bridge disc. Its ISO9660 layer is a stub — on Server
  2022 media the entire ISO9660 root holds one file, `README.TXT`. Everything real is
  UDF-only.
- `sources/install.wim` is 4.04 GiB, over ISO9660's 4 GiB per-file ceiling. Any
  ISO9660-only reader structurally cannot read it.
- The WIM image list is a ~12 KB XML resource at a fixed offset near the end of the
  file. Reading it is two seeks; there is nothing worth caching.
- Retail media drops the `Eval` suffix from `EDITIONID` and `FLAGS`, so never match on an
  exact edition string or a fixed image index.
- Windows Setup insists `install.wim` sit on the volume it booted from. Point
  `<InstallFrom><Path>` at a second partition and it fails to resolve the licence terms.

### The media identifies itself

Each `<IMAGE>` in that XML resource carries `ARCH`, `PRODUCTTYPE`, `BUILD` and
`INSTALLATIONTYPE`. Verified by reading real media: Server 2019/2022/2025 evaluation,
Windows 11 22H2 retail, Windows 10 22H2 evaluation. `cargo run -p oxwin-cli --example
wimlist -- <iso>` prints all of it.

- **`ARCH` is 9 for amd64 and 12 for arm64.** Nothing in this workspace works on arm64 —
  the drivers are amd64 and the answer file hardcodes `processorArchitecture="amd64"` — so
  arm64 media has to be refused rather than built into an unbootable image.
- **`PRODUCTTYPE` is `ServerNT` or `WinNT`.** This is the only structural client/server
  signal. Do not sniff for "Server" in an edition name.
- **Server 2025 and Windows 11 24H2 are both build 26100.** `BUILD` alone cannot tell them
  apart, and getting it wrong means the wrong answer file with the hardware-check bypasses
  in the wrong state. Always pair `BUILD` with `PRODUCTTYPE`.
- **`BUILD` is the base build, not the patch level.** Media whose filename says `19045`
  reports `19041`. Match a base build or a range, never the number the filename implies.
- **`INSTALLATIONTYPE` (`Server` / `Server Core` / `Client`) is the real Core marker**, per
  image. The `flags`-ends-in-`core` heuristic is wrong on client media: Windows 11 Home is
  `EDITIONID=Core`, `FLAGS=Core`, so it classifies as Server Core, while `Home N` (`CoreN`)
  does not. Keep the heuristic only as a fallback for media omitting the tag.
- **`EDITIONID` and `FLAGS` disagree about casing**, identically on all three server
  releases: `ServerDatacenterEval` against `ServerDataCenterEvalCore`. Lowercase before
  comparing.
- **Evaluation media ships an `ei.cfg`; retail and volume-licensing media does not.**
  Verified on all three server evaluations (present) against Windows 11 retail and the
  Server 2022 volume-licensing DVD (both absent). We generate our own either way, so this
  matters for what Setup would have done unaided, not for what we write.
- **`[VL]` in the `ei.cfg` we generate is `0`.** Every evaluation ISO ships `[VL] 0` and
  non-evaluation media ships no `ei.cfg`, so there is no evidence it should be anything
  else. `[Channel]` is the field that matters.
- **`[Channel]` must name a licence directory that exists in the image.** Setup reads
  `\Windows\System32\<lang>\Licenses\<Channel>\<EditionID>\license.rtf`; a miss is
  `SkuGetImageEulaAsString ... hr = 0x80070490`, which reads as "Windows cannot find the
  Microsoft Software License Terms". Verified with `wimlib-imagex dir`: evaluation media
  carries only `Eval`; volume media carries `Eval OEM Volume _Default`; retail Windows 11
  carries `OEM Volume _Default`. **No media carries `Retail`**, so `builder::license_channel`
  emits `Eval` for evaluation media and `_Default` for everything else, and never `Retail`.
- **macOS mounts UDF case-sensitively.** `ei.cfg` is `EI.CFG` on the media, so a mounted
  path has to be matched case-insensitively — `Source::find` does for the ISO route and
  `read_dir` does not for the mount route.
- **Windows 10 and 11 Home have no RDP host** and cannot domain-join. RDP is one of the
  three ways into a guest, so Home plus RDP is a warning, not a working configuration.
- Server 2019, 2022 and 2025 share one edition vocabulary: the same four images, the same
  `ServerStandardEval`/`ServerDatacenterEval` ids, differing only in the year in the name.
  One target parameterised by year, not three tables.

## QEMU cannot test everything

A QEMU harness honours guest NVRAM, so after an install it re-enters the installed OS
directly and **structurally cannot exercise the chooser's second branch**. The VPC
firewall does not exist locally either, so RDP appears to work in QEMU and then times
out on a rack. Both need real hardware.

## Screenshots — read this before capturing

`screencapture -x file.png` grabs the **entire display**, including whatever meeting,
message or document the user has open. That happened once and caught a video call with
four colleagues on camera.

Capture the app's rectangle only:

```
OXWIN_WINDOW_AT=120,120 cargo run -p oxwin-gui &
screencapture -x -R 122,150,976,688 shot.png     # inset inside the window
```

`OXWIN_WINDOW_AT` exists for exactly this. Accessibility permission is not granted, so
`osascript` cannot read window bounds — pin the position instead.

`OXWIN_START_STAGE=<0-4>` jumps to a stage for review and is compiled out of release
builds. Stages 3 and 4 need a finished build to render; seed one temporarily and **remove
the scaffolding before handing the app back**.
