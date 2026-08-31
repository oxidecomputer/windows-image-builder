# Plan

Goal: someone with a laptop and a rack connection can get Windows running, without a
spare x86 Linux box and without hand-writing an answer file.

Status: **v0.1, v0.2, v0.3 and v0.4 are code-complete.** The engine works, the payload travels inside
the binary, CI builds on three platforms, and the release is read out of the media rather
than asserted by the user — five releases, each offering the editions its ISO actually
carries.

Four of those five have installed on a rack and been checked: Server 2019, Server 2022
(evaluation and volume licensing) and Windows 10. **Server 2025 and Windows 11 have not**,
and the reason is not this builder — see [the two blockers](#what-this-does-not-do).

Uploading now happens inside the app, over Oxide's own SDK, and the app can create the
instance too — so the flow no longer hands anyone a command to paste unless they want
one. That path has been run against a real rack; see
[what the first real upload taught us](#what-the-first-real-upload-taught-us).

**v0.4 is code-complete and has run on a rack.** `oxwin golden` drives media to a
reusable Windows image in one resumable command, and a clone of that image has been
booted and checked from inside the guest. Doing it found a defect that made every
golden image this tree had ever produced wrong — `sysprep.exe` does not set
`$LASTEXITCODE`, so the guard against an infinite generalize loop *was* the loop.

Next is signed artefacts, which is a procurement problem more than a code one, and the
GUI surface for the golden flow.

---

## v0 — built

The five-stage GUI, working end to end against the existing builder.

- [x] Workspace: `oxwin-core` / `oxwin-gui` / `oxwin-cli`, with the core holding no UI
- [x] Stage indicator in the brand palette, with revisitable completed stages
- [x] Image Selection: drag-and-drop or browse, ISO or mounted folder, with a size sanity check
- [x] Settings: golden image vs named machine, SSH-key-only vs password, access and
      driver toggles, product key
- [x] Validation in the core, surfacing blocking problems and warnings separately
- [x] Processing: live progress with ETA, cancel, and a log pane
- [x] Export: real file save, plus generated `oxide` CLI commands
- [x] Guided Install: numbered checklist with commands filled in from the chosen names
- [x] ISO reading with no mount at all, and any already-mounted directory read as-is
- [x] No default password anywhere in the workspace
- [x] A suite that needs no hardware to run — 142 tests at the time, 183 today

---

## v0.1 — the gaps a demo will expose

- [x] **GitHub Actions CI.** `fmt`, `clippy -D warnings`, `build` and `test` on macOS,
      Linux and Windows. All three, not one, because the app ships on all three and the
      goldens have to hold on each — a path separator or an endianness slip in a generated
      volume shows up nowhere else. A separate job fetches the payload and builds with
      `OXWIN_REQUIRE_PAYLOAD=1`, so the embedding path is exercised on every PR.
- [x] **Embedded assets.** `build.rs` walks `assets/` and `include_bytes!`s every file. A
      missing payload embeds an empty table so `cargo test` still works on a fresh clone;
      `OXWIN_REQUIRE_PAYLOAD=1` makes it a hard failure for release builds.
- [x] **Released artifacts.** A `v*` tag builds the GUI and CLI for macOS (universal),
      Windows x86_64 and Linux x86_64. The payload is fetched once and shared, and the run
      fails if the three artifacts do not report the same payload fingerprint.
- [x] **Dependabot.** `cargo` and `github-actions` ecosystems, weekly, grouped so the egui
      tree arrives as one PR rather than forty — patch bumps in one group, minor bumps in
      another, because egui makes breaking API changes at every minor release and this
      project already had to be written against 0.36's reworked API. A minor egui PR is a
      porting task, not a merge button, and the split is what says so in the title.
      Nothing is ignored: grouping handles the noise, and a dependency reaching 1.0 should
      reach a human rather than be filtered out.
- [x] A real window icon and an app name that is not the binary name. The icon is
      *drawn*, in `oxwin-gui/src/icon.rs`, rather than committed as a `.png` — a binary
      blob is the one artefact here that cannot be reviewed by reading, and the mark is
      simple enough (three stacked bars, the partitions of the image, brightening upwards)
      to be forty lines of arithmetic. `with_app_id` gives Wayland and GNOME something
      better than `oxwin-gui` to label the window with.
- [x] **Decide whether the payload may be redistributed.** **Yes.** Everything embedded is
      open source and redistributable: virtio-win (Red Hat, BSD/GPL), Win32-OpenSSH
      (Microsoft, BSD-style), and efifs / UEFI-Shell / UEFI:NTFS (GPLv3 and BSD-2). No
      Microsoft closed code is in the payload, and none can be — the Windows installer
      arrives as an ISO the user supplies, is never fetched by us and is never part of an
      artifact we publish. GPLv3 obliges us to offer the corresponding source for the
      efifs and UEFI:NTFS binaries; `fetch-payload.sh` already pins the upstream release
      each was taken from, which is where that offer points.

## v0.2 — the release matrix

Server 2019, 2022, 2025, Windows 10 and Windows 11, all installable and all offering the
editions their media actually carries.

The obvious way to do this is a wider radio button: add the releases to
`WindowsRelease::ALL`, write a target for each, done in an afternoon. That was rejected.
A radio button is an assertion by the user, and nothing checks it — pick "Server 2022",
hand it a Windows 11 ISO, and the answer file carries Server edition names with the
hardware-check bypasses switched off. It fails the way everything in this project fails:
the build succeeds and the install looks fine.

**So the release is detected, not chosen.** The media already knows what it is, and we
already parse the file that says so.

### What the media tells us

Verified against real ISOs — three server evaluations, Windows 11 22H2 retail, Windows 10
22H2 evaluation. Every field below comes from the `<IMAGE>` elements of the WIM XML
resource, which the engine already reads to pick an edition:

| Field | Values seen | What it decides |
|---|---|---|
| `ARCH` | `9` amd64, `12` arm64 | Whether we can build at all |
| `PRODUCTTYPE` | `ServerNT`, `WinNT` | Server vs client, structurally |
| `INSTALLATIONTYPE` | `Server`, `Server Core`, `Client` | Desktop vs Core, per image |
| `BUILD` | 17763, 19041, 20348, 22621, 26100 | Which release, with `PRODUCTTYPE` |

Four things this turned up, each of which would have cost a day later:

- **Server 2025 and Windows 11 24H2 are both build 26100.** Only `PRODUCTTYPE` separates
  them. Detection keyed on the build number alone installs the wrong answer file with
  total confidence.
- **`BUILD` is the base build, not the patch level.** The Windows 10 media whose filename
  says `19045` reports `19041`. Match on a base build or a range; never on the number the
  filename implies.
- **`INSTALLATIONTYPE` replaces the Core-ness heuristic, and fixes a live bug.**
  `is_core_image` tests whether `flags` ends in `core` — and Windows 11 Home is
  `EDITIONID=Core`, `FLAGS=Core`, so today it is classified as a Server Core image. Home N
  (`CoreN`) is not. The heuristic is meaningless on client media. It stays only as the
  fallback for media omitting the tag, with a test saying why.
- **`EDITIONID` and `FLAGS` disagree about casing**, consistently, on all three server
  releases: `ServerDatacenterEval` against `ServerDataCenterEvalCore`. Pin it with a test;
  it is exactly the exact-match trap the media notes warn about.

Retail and volume-licensing media do not touch any of the four fields above — confirmed by
reading the Server 2022 volume-licensing DVD, which reports the same `ARCH 9` / `ServerNT`
/ `20348` / `Server` as the evaluation media. Only the `Eval` suffix on `EDITIONID`/`FLAGS`
changes, which is already what `builder.rs` uses to pick the `ei.cfg` channel.

The other difference is presence, not content: **evaluation media ships an `ei.cfg` and
non-evaluation media does not** (verified: three server evaluations have one, Windows 11
retail and the Server 2022 VL DVD do not). We write our own regardless, so this describes
what Setup would do unaided rather than anything we depend on.

### The work

- [x] **Verify the driver payload actually has these targets.** `fetch-payload.sh` fetches
      `2k22` and `w11`; the matrix needs `2k19`, `2k25` and `w10` too. It currently *skips*
      a missing target with a log line and fails only when nothing at all matched, so a
      target that does not exist upstream yields a payload with no drivers for that release
      and a build that looks fine. Make a requested-but-missing target a hard failure, then
      run it. This is step one because everything else assumes it.

      Done, and it found something: virtio-win 0.1.285 ships **two** distinct driver sets
      across these five targets — `2k19`, `2k22` and `w10` are byte-identical to each
      other, and `2k25` and `w11` are byte-identical to each other. So the `_ => "2k22"`
      fallback that used to choose the driver directory was harmless for Server 2019 and
      Windows 10 by coincidence, and wrong for Server 2025. Nothing about that coincidence
      is promised by a future virtio release.
- [x] **`wim::Image` gains `arch`, `build`, `product_type` and `installation_type`**, and
      `is_core_image` becomes a field test rather than a string test.
- [x] **New `oxwin-core/src/media.rs`,** one entry point: `inspect(&Media) -> MediaInfo`,
      carrying the architecture, the detected release, the real image list and whether
      `EI.CFG` is present. It runs when the ISO is picked, not when the build starts —
      stage 2 can only offer real choices if the media has already been read. A UDF walk
      plus two seeks; sub-second on all five ISOs tested.
- [x] **`WindowsRelease` gains `Server2019` and `Windows10`,** and `target_for` loses its
      `Server2025` bail. The three server releases share one edition vocabulary — same four
      images, same `ServerStandardEval`/`ServerDatacenterEval` ids, differing only in the
      year — so they are one target parameterised by year, not three copy-pasted tables.
      `bypass_hardware_checks` keys off client-ness rather than a hardcoded `Windows11`.
- [x] **Arm64 media is refused** with a message naming the architecture. Today it builds an
      unbootable image in silence: the drivers are amd64 and the answer file hardcodes
      `processorArchitecture="amd64"`.
- [x] **An unrecognised build stays buildable, with a warning** naming the build number and
      the driver directory we guessed. Server 2016 and whatever ships in 2027 should not be
      walled off by a table we forgot to update.
- [x] **Stage 2 shows the media's own image list.** This is the "edition picker driven by
      the WIM's own image list" item, pulled forward from Later because detection makes it
      nearly free — the list is already in hand. It replaces two controls: the release radio
      (now a detected fact, displayed) and the Desktop/Core radio (now a property of the row
      you picked, and absent on client media). It has to read well at both extremes: the
      Windows 11 22H2 retail ISO carries eleven images, the Windows 10 evaluation exactly
      one.
- [x] **`Experience` and the `edition` string leave the user-facing surface.** `edition` is
      today a hidden `Draft` field fixed at `"datacenter"` that no widget ever writes, and
      `Settings::edition_hint` appends `"core"` where `unattend::Config::from_settings`
      appends `"-core"` — two conventions for one fact. `Settings` keeps `release` as a
      required field rather than an `Option`, so it stays a complete serialisable
      description of a build; the GUI fills it from detection and `--windows=` becomes an
      override that warns on mismatch instead of the primary input.
- [x] **Warn that Home has no RDP host.** Windows 10 and 11 Home cannot accept Remote
      Desktop and cannot domain-join. RDP is one of the three ways into a guest, so
      selecting Home with RDP enabled has to say so rather than be discovered on the rack.
- [x] **Settle `[VL]` in the generated `ei.cfg`.** Settled, and the question was aimed at
      the wrong field. `[VL]` stays `0`: every evaluation ISO on hand ships `[VL] 0`, and
      volume-licensing media ships no `ei.cfg` at all, so there is nothing suggesting
      otherwise.

      `[Channel]` was the broken one. Setup resolves the EULA from
      `\Windows\System32\<lang>\Licenses\<Channel>\<EditionID>\license.rtf` inside
      the image, so the value has to name a directory that is really there. Listing the
      WIMs with `wimlib-imagex dir`:

      | Media | Channel directories present |
      |---|---|
      | Server 2022 evaluation | `Eval` |
      | Server 2022 volume licensing | `Eval` `OEM` `Volume` `_Default` |
      | Windows 11 22H2 retail | `OEM` `Volume` `_Default` |
      | Windows 10 22H2 evaluation | `Eval` |

      **No media carries a `Retail` directory** — not even the retail Windows 11 ISO — and
      `Retail` is exactly what we emitted for anything without an `Eval` suffix. That is
      the "Windows cannot find the Microsoft Software License Terms" failure, waiting for
      the first person to build from non-evaluation media. Now `_Default`, Microsoft's own
      catch-all, which is populated for the exact `EDITIONID` on both non-evaluation
      samples. Evaluation media is untouched, so the reference image is byte-identical.
- [x] **A golden per release,** and the casing and collision cases pinned by tests.

      One unattend golden per release, each with `image_index: None` — with an index set
      the edition table never reaches the answer file, so every one of them would have
      been byte-identical to `defaults` while appearing to test the matrix. What they pin
      is the `/IMAGE/NAME` string Setup matches on, and the LabConfig bypasses being
      present on the two client releases and absent on the three server ones.
      `every_release_has_a_golden` iterates `WindowsRelease::ALL`, so the next release
      added cannot arrive unpinned.

      The other three cases were already covered and stayed that way: the
      `ServerDatacenterEval`/`ServerDataCenterEvalCore` casing split in `wim.rs`, the
      build-26100 collision in `media.rs`, and the licence channel — including
      volume-licensing media — in `builder.rs`. The line that used to stand here said a
      VL case should expect `Channel=Retail`; that was the bug, not the plan. It is
      `_Default`, and no media anywhere carries a `Retail` licence directory.

### The gate

For Server 2022 Datacenter Desktop Experience, the image this produces must be
**byte-identical to the one it produces today**. `builds_the_same_image_by_every_route` and
the committed media reference already give us that check for free, and it is what makes a
refactor of this size safe rather than hopeful. In `dump_goldens`, any hunk outside the
genuinely new per-release cases is a bug being blessed.

The gate has to be run against **the** Server 2022 ISO, not *a* Server 2022 ISO: the
goldens hardcode an `install.wim` of 4340202461 bytes, so a different ISO of the same
release puts files in different clusters and cannot reproduce them. See
[TESTED-MEDIA.md](TESTED-MEDIA.md), which identifies it.

### What this does not do

Detection makes the releases *correct*. It does not make them *verified* — that still means
an install on real hardware, per release, checking the artefact rather than the outcome:
`C:\oxide-bootstrap.log` present, the NIC bound, the intended edition installed. Windows 11
additionally cannot be fully exercised in QEMU, because the hardware-check bypasses are
only interesting on firmware that lacks a vTPM.

- [x] Server 2019 installed and verified on a rack
- [ ] Server 2025 installed and verified on a rack — blocked twice over: propolis#1199
      (guest sees no disks; diagnosed and fixed) and an NVMe controller fault on the rack
      itself (crashes this release; reported 2026-08-27, not yet characterised). Landing
      the first does not clear the second.
- [ ] Windows 11 installed and verified on a rack — the same two blockers
- [x] Windows 10 installed and verified on a rack
- [x] Server 2022 **re-verified** from the current tree on 2026-08-27, after `pvpanic`
      and `viosock` joined the payload and `UpgradeData` joined the answer file. Every
      device in the guest has a driver. This is the row that makes the others credible:
      those three changes altered the bytes of every image and every answer file, so the
      earlier installs were verification of a tree that no longer existed.
- [x] Retail media confirmed alongside evaluation media for at least one server and one
      client release — the `Eval` suffix is the one thing that moves

## v0.3 — upload from inside the app

Stage 4 offers three paths, and they are alternatives rather than a sequence. Today the
second and third are commands to paste.

| Path | What it does | Ends at |
|---|---|---|
| **Save the image file** | Writes the image wherever the user wants | A file |
| **Upload as a disk** | Creates and uploads the installer disk, under a name they choose | A disk on the rack |
| **Create the whole instance** | The above, plus the blank system disk and the instance | A booting instance |

**Save-to-file stays exactly as prominent as the other two.** An airgapped user has to be
able to get an image and install it without this app touching a network, and the moment
the automated path looks like the main one, that stops being true.

### The client: the `oxide` SDK

`oxide` 0.18, MPL-2.0 like this workspace, version-stamped to the rack API it was
generated from (`0.18.0+2026073100.0.0`). It also parses the same
`~/.config/oxide/credentials.toml` that `oxide auth login` writes, so the profile picker
is nearly free and **no token is ever read, held or logged by us**.

Not `oxide-api` on crates.io. That is an abandoned release candidate whose tree is
reqwest 0.11, hyper 0.14, rustls 0.21 and uuid 0.8 — a stale parallel TLS stack that
Dependabot could not move, because the SDK pins it. The live crate is `oxide`, and its
149 dependencies are all current.

### The crate: `oxwin-rack`, not `oxwin-core`

The SDK brings tokio, reqwest and rustls. `oxwin-core` has four dependencies and produces
bytes deterministically; that is a property worth keeping, and it is also why its golden
suite runs in a fifth of a second. So the network code is a new crate, which the GUI and
the CLI both depend on.

`oxwin-rack` owns a current-thread runtime internally and exposes a **blocking** API that
emits `progress::Event`. The async never crosses the crate boundary, so the GUI's existing
build-thread model is unchanged and the core's rule — never print, never block on a
human — is inherited rather than reargued.

### The CLI is a first-class consumer, not an afterthought

Someone with nothing but a Linux or Windows terminal has to be able to pass the variables
in and get an image out, or get an image uploaded. That is the same constraint that keeps
`println!` out of `oxwin-core`, applied one crate over, and it rules out a category of
design that would otherwise feel natural:

- **No TTY assumptions anywhere in `oxwin-rack`.** No prompt, no picker, no spinner, no
  reading from stdin. The profile is a parameter; the GUI's picker is a GUI feature that
  fills that parameter in.
- **Every variable settable by flag or environment**, because a script cannot click.
- **Progress is `progress::Event`,** which the GUI renders as a bar and the CLI renders as
  plain lines — or, under `--quiet`, not at all.
- **Exit codes and stderr carry the outcome**, so a failed upload is detectable by a shell
  rather than only readable by a human.

`oxwin-cli` grows `upload` and `instance` subcommands alongside `build`, and they have to
be written in the same pass rather than retrofitted. A GUI-shaped API that is later bent
into a CLI is exactly the rewrite the core's architecture rule exists to prevent.

### The work

- [x] **`oxwin-rack`,** wrapping the SDK: profile discovery, disk create, bulk write,
      finalize. Blocking, event-emitting, no async in its public API. Instance creation
      is still to come.
- [x] **`oxwin-cli upload`,** reaching the same code the GUI will, written in the same
      pass as the crate rather than after it.
- [x] **Zero-block skipping.** Proven on the first real upload: **2.18 GiB of a 7 GiB
      image never went on the wire**, 31% of it.
- [x] **Profile discovery,** including the environment. No token ever leaves
      `profile.rs`; a `Selector` is handed to the SDK instead. See the note there about
      `OXIDE_TOKEN` being consulted only when no profile is named — naming one
      unconditionally, the obvious implementation, silently breaks CI.
- [x] **Cancellation runs the teardown**, and the CLI installs a Ctrl-C handler so it
      actually gets the chance. Verified by interrupting a real upload: the disk lands in
      `import_ready` rather than the undeletable `importing_from_bulk_writes`. `finalize`
      deliberately does *not* run on failure — a half-written disk must not be presented
      as a complete one.
- [x] **The installer disk is created with a 512-byte block size explicitly.** Confirmed
      against the rack: `block_size: 512`, `size: 7516192768`, exactly 7 GiB.
- [x] **Instance creation**, pinning `boot_disk` and requesting an ephemeral external IP.
      Both are load-bearing: the SDK's own documentation says an instance with no boot
      disk "may result in an instance that only boots to the EFI shell", and nothing
      inside a guest can give itself an external IP afterwards. The tcp/3389 warning
      rides along in `Created::warnings`, because it is the most common rack surprise and
      is not fixable from the answer file.
- [x] **Stage 4 in the GUI**: the three paths, with the profile picker as a `ComboBox`
      (the number of logins is data, and a control sized by data pushes the rest of the
      form around). The copyable commands are still there, under a disclosure — an
      airgapped rack must never depend on this app reaching a network.
- [x] **Partial failure names what exists**, in both the CLI and the GUI, with the
      commands to clear it. `Leftovers::cleanup_commands` orders instances before disks,
      because a disk attached to an instance cannot be deleted and the other order hands
      the user a command that fails on the first line.

### What the first real upload taught us

None of this was reachable from a test. Every offline test passed at both speeds,
because the chunking is identical either way.

**The upload path is verified against a genuinely remote rack**, with the operator a
continent away from it. That is good coverage rather than a caveat: a long-haul link is
the hard case, and the numbers below are dominated by that distance. A rack on the same
network has far less round-trip time to hide, so read these as "this works from
anywhere", not as how fast a rack is.

- **Serial chunk upload was the single biggest defect.** Each 512 KiB write waits for its
  own response, so throughput follows round-trip latency and extra bandwidth buys
  nothing. Sixteen concurrent writes moved 7 GiB in 8:44 over the same link on which
  serial writes managed a small fraction of that. `CONCURRENCY` carries the reasoning in
  its doc comment so nobody simplifies it away.
- **It is HTTP/1.1, so concurrency means connections.** 16 in flight showed as exactly 16
  established TCP connections, and under HTTP/2 they would have multiplexed onto one.
  They are pooled and reused — the same 16 local ports across the whole upload, so there
  are 16 TLS handshakes in total and none per chunk. But HTTP/1.1 cannot multiplex, so
  each connection is strictly one request at a time, and more concurrency is the only
  lever left. Raising it is worth measuring, especially on a high-latency link.
- **The wire carries a third more than the image.** The endpoint takes base64 in a JSON
  body, so 7 GiB of image is about 9.3 GiB of request. Inherent to the API, not a choice
  of ours, and worth knowing before anyone attributes the timing to the client or the
  rack.
- **The progress bar could never reach 100%.** It counted bytes *sent* against the size of
  the image, so on anything with zeroes in it the bar stopped at the non-zero fraction —
  68% on the first real run, with the upload finished. That is how someone concludes an
  upload has hung and kills it, which mid-import strands a disk. `Progress` is now a
  separate type precisely because the arithmetic lives where no test could reach it: it
  owns both counters so a caller cannot combine them wrongly.

### What v0.3 deliberately does not do

**It does not wait for the install to finish, or detach the media afterwards.** Three
reasons, and the first is decisive:

- **The media is already safe to leave attached.** The UEFI Shell chooser detects an
  installed Windows and boots it, every time from then on. That is the whole reason the
  chooser exists, given that boot order belongs to the control plane and has no
  fallthrough. Detaching is tidiness, not correctness.
- **A SAC prompt cannot be detected on client media at all.** Windows desktop editions
  never write anything to the serial console. A watcher keyed on it would work on server
  media and hang forever on Windows 10 and 11 — this project's characteristic failure,
  built on purpose.
- **The timescale is wrong.** Server 2022 reaches a login prompt in tens of minutes across
  several reboots; 2025 and 11 are slower still. "Five minutes" is not the shape of it.

If "tell me when it is usable" is worth building, the signal is **SSH reachability on
tcp/22**: true on every edition, no serial parsing, already permitted by the default VPC,
and it means the guest can actually be logged into rather than that a screen appeared.
That belongs in v0.4 with the rest of the long-running orchestration, where resumability
is already an item — a watcher that cannot survive a laptop lid closing is the wrong thing
to build first.

## v0.4 — the golden image, automated

The most valuable feature and the least demoable: roughly an hour of wall clock.

- [x] Drive the whole cycle: upload → install → `sysprep /generalize /shutdown` →
      snapshot → image → delete the temporary instance and disks.

      `oxwin golden <iso-or-img> --run=<name> --project=<p>` does all of it, and the
      individual steps are `watch`, `snapshot`, `image`, `teardown` and `verify`.
      **Run end to end against a rack on 2026-08-31**: Server 2022 evaluation media,
      port 22 at 4m52s, generalized and stopped at 6m20s, image made, instance and
      both disks and the snapshot removed.

      Doing it turned up a defect that made every golden image built by this tree
      wrong, and it was not in the new code: **`sysprep.exe` does not set
      `$LASTEXITCODE`**, so the guard that was supposed to prevent an infinite
      generalize loop *was* the loop. See the trap in CLAUDE.md. No golden image
      produced before that fix should be trusted.

      For the record, because the previous wording was wrong for a while:
      It sets `<ComputerName>*</ComputerName>` and nothing else. `*` resolves once,
      during `specialize`, so a clone of the finished disk keeps the name that was
      picked *and* the machine SID — the exact collision a golden image exists to
      prevent. Windows re-resolves it only after `sysprep /generalize`. The UI, the
      README and the `Deployment::GoldenImage` doc comment now say so and give the
      command, because the previous wording ("each clone gets its own random computer
      name") was true only of a step nobody was told to run.

      A golden build now registers `OxideGeneralize`, a SYSTEM scheduled task that fires
      at startup, waits until `SystemSetupInProgress` and `OOBEInProgress` are both zero,
      then syspreps and shuts down. Three things about that shape are deliberate:

      - **A SYSTEM task, not `FirstLogonCommands`.** The latter needs an interactive
        logon, which means turning autologon on — trading a cloning problem for a console
        session nobody is watching. Autologon stays off in every shipping path.
      - **A marker file written before sysprep runs**, so it is captured into the image.
        Without it every clone would generalize itself and shut down on first boot: a
        fleet that turns itself off.
      - **`/unattend:` is passed explicitly**, naming a copy of the answer file that
        still has the password in it. Without it sysprep falls back to the cached copy,
        whose password Windows has replaced with `*SENSITIVE*DATA*DELETED*`, and the
        machine stops at the out-of-box wizard. Found on a rack, not in a test: the
        guest had an IP, working drivers and no reachable RDP, which reads as a
        networking fault and is not one.
      - **Failure undoes the marker and re-registers the task.** `sysprep /shutdown` does
        not return on success, so reaching the next line means it failed — and a machine
        that looks generalized and is not is worse than one that retries.
- [x] Verify a clone of that image actually boots. Until that is proven, "golden image"
      is a claim, not a feature.

      `oxwin verify <run>` builds the clone and requires it to come up **and stay up
      for five minutes**. The staying is the test, not the coming up.

      Done on a rack, 2026-08-31, and checked from inside the guest rather than from
      the exit code. `C:\Windows\Panther\UnattendGC\setupact.log` on the clone was
      written at 20:07, after the original had stopped at 19:59, and shows
      `msoobe.exe` loading the ComputerName plugin and reaching
      `SETACTIVE: wizard page for page End` — so OOBE genuinely re-ran on the clone,
      assigned a fresh name, and completed rather than stalling. `oxide-generalized.txt`
      was captured into the image and `OxideGeneralize` is not registered on the clone,
      so it will not generalize itself.

      **The settle window earned its place immediately.** The clone answered on port 22
      after 36 seconds — because sshd starts automatically and answers while OOBE is
      still running, minutes before the machine is configured. A check that finished at
      the first successful connection would have passed before the guest had finished
      setting itself up.
- [x] Resumability. An hour-long operation that cannot be resumed after a laptop lid
      closes is not finished.

      There is no journal. Every resource name derives from `--run`, and every step
      asks the rack what already exists, so **resuming is re-running the identical
      command** — which is also the recovery instruction printed after any failure. A
      journal would have been an assertion nothing checks: delete a disk by hand and
      it still claims the disk is there, and a run started on a laptop could not be
      finished from anywhere else. Exercised on a rack by resuming a run whose
      installer disk was already uploaded; it skipped straight to the watch.
- [x] **Know when an install has finished, by polling tcp/22.** Deferred here from v0.3.

      With one correction the plan had wrong: **for a golden run the signal is the
      instance reaching `stopped`, not port 22.** The guest finishes by shutting
      itself down, a guest shutdown stops the instance and a guest reboot does not,
      and Setup reboots several times. Port 22 earns its place for one judgement —
      an instance that stops *without it ever having answered* did not finish, because
      a guest that dies during Setup also stops the machine. Without that
      discrimination a failed install and a finished golden image are the same
      observation.
      SSH reachability is the one completion signal that works on every release: desktop
      editions write nothing to the serial console, so a SAC-prompt watcher would hang
      forever on Windows 10 and 11 while appearing to work on server media. It is also the
      more useful claim — the guest can be logged into, not merely that a screen appeared.
      Port 22 is already open in the default VPC, so it needs no firewall change. Once
      this exists, detaching the installer disk automatically becomes reasonable; until
      then it stays manual, and harmless, because the chooser boots the installed Windows
      regardless.

## v1 — the engine — done

Ported module by module from a JavaScript builder, each step gated on byte-for-byte
agreement, and finished: media built by this engine, through the GUI, installed Windows on
an Oxide rack first try. What that took, what it found, and the three reproducibility bugs
it surfaced are in [DEVELOPMENT.md](DEVELOPMENT.md).

The reference is now the committed goldens in `crates/oxwin-core/testdata/`, generated by
that builder before it was left behind. It is not coming back, and it does not need to.

Two decisions from that work worth not relitigating:

- **A FAT32 media mode stays unported.** It needs `wimlib` to split `install.wim`, answers
  a question `ei.cfg` has since answered, and no media anyone booted came out of it.
- **The whole-image comparison is not total coverage.** It cannot see a wrong sizing
  headroom factor, because every factor from 1.1 to 1.2 rounds our payload to the same
  volume. That needed its own unit test.

## v1 — packaging

Deliberately after the release matrix. Signing needs an Apple Developer ID and a Windows
certificate in repository secrets — a procurement and access problem, not a code one — and
it blocks nobody who builds from source. The releases people actually want to install are
worth more than a first run without a Gatekeeper prompt.

- [ ] macOS: a signed and notarised `.app`, plus a DMG. The mac artifact is a bare binary
      today, so a first-time user meets Gatekeeper. Needs the Developer ID in secrets.
- [ ] Windows: a signed executable. Needs the signing certificate in secrets.
- [ ] The CLI, which should fall out of the core almost for free — and if it does not,
      the core has drifted from its rule.

## Later

- [ ] Offer to add the `tcp/3389` VPC firewall rule when RDP is enabled, rather than
      only warning about it
- [ ] Localised media. Detection is language-agnostic — `PRODUCTTYPE` and
      `INSTALLATIONTYPE` are not translated — but no non-English media has been read, and
      the edition *names* certainly are translated.
- [ ] Arm64, should Oxide ever offer an Arm64 sled. v0.2 refuses it; the refusal is the
      honest answer until there is somewhere to run it.

---

## Open questions

- **Where does the boot-disk hand-off end up?** Leaving the installer attached works
  because the UEFI chooser detects an installed Windows and boots it. It is the simplest
  answer and it is verified. The alternative — the app orchestrating a one-time boot
  order change — was rejected because it breaks airgapped use.
- **Is `sysprep` enough for a true golden image on Oxide?** Unverified. The clone-boots
  test in v0.4 is what answers it.
- **Should password login exist at all?** It is currently allowed with warnings. If the
  golden-image flow becomes the main path, key-only may be worth enforcing outright.
- **Is a Windows 10 target worth carrying past its end of support?** It went out of support
  in October 2025. It is in the matrix because it costs almost nothing once client detection
  exists and people plainly still run it, but it is the first thing to drop if the release
  table becomes a burden.
- **What does the app do when the media carries no image list?** Detection assumes a
  readable `sources/install.wim`; a mounted directory is fine, since the WIM is still
  there, but a stripped or half-copied source has nothing to read. The fallback would be
  an asserted release, which is the input this section just removed. Probably: build
  allowed, every release-dependent choice explicit and warned, and `--windows=` required
  rather than optional.
