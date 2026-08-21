# Tested media

Which Windows ISOs this has actually been run against, so the answer to "I have
`en-us-random-server-2027.iso`, will it work?" is a lookup rather than a guess.

**Do not read a blank row as "broken".** Most media will work; the table records what has
been *demonstrated*, and the honest answer for anything absent is "nobody has tried it".

## How to identify your media

Filenames are unreliable — they are chosen by whoever downloaded the ISO, and Microsoft
ships the same release under several. Identify media by what is inside it:

```
cargo run --release -p oxwin-cli --example wimlist -- /path/to.iso
```

That prints the fields below, along with the release it detected and anything it would
refuse or warn about. Every row in the tables here has been run through it. Compare them against the table; matching `PRODUCTTYPE`,
`BUILD` and the edition ids means the media is equivalent to a tested row even if the
filename differs. `install.wim` size is included because it distinguishes otherwise
identical-looking releases — the two Server 2022 evaluation ISOs below differ only there.

## What "tested" means

Three separate claims, deliberately not collapsed into one column. A finished install
proves nothing on its own, so each level is only as good as what was checked:

| Level | Means |
|---|---|
| **Read** | Detection verified: architecture, release and the edition list parse correctly |
| **Built** | An installer image was produced from it end to end |
| **Installed** | Booted on an Oxide rack and installed, artefact checked — `C:\oxide-bootstrap.log` present, NIC bound, intended edition |

## Server media

| Release | Channel | `install.wim` | BUILD | Editions | `ei.cfg` | Level |
|---|---|---|---|---|---|---|
| Server 2019 | Eval | 4309326283 | 17763 | `ServerStandardEval`, `ServerDatacenterEval` | present | **Installed** |
| Server 2022 | Eval | **4340202461** | 20348 | `ServerStandardEval`, `ServerDatacenterEval` | present | **Installed** |
| Server 2022 | Eval | 4857525383 | 20348 | `ServerStandardEval`, `ServerDatacenterEval` | present | Read |
| Server 2022 | Volume licensing | 4857802167 | 20348 | `ServerStandard`, `ServerDatacenter` | absent | **Installed** |
| Server 2025 | Eval | 5189163060 | 26100 | `ServerStandardEval`, `ServerDatacenterEval` | present | Built |

All server media reports `ARCH 9` (amd64) and `PRODUCTTYPE ServerNT`, and carries four
images: Standard and Datacenter, each as `Server` and `Server Core`.

The `ei.cfg` column is what the media ships, not what we write — we generate our own
either way. The one thing it changes is the licence channel: evaluation media carries only
an `Eval` licence directory inside `install.wim`, while volume and retail media carry
`OEM`, `Volume` and `_Default` and **no `Retail`**. See `builder::license_channel`.

**The bold row is the reference media.** `install.wim` of 4340202461 bytes is hardcoded in
`exfat.rs` and `testdata/exfat/placements.json`, so every committed golden — and the image
that installed on a rack first try — came from that specific ISO. It is the one to use when
checking that a change leaves the output byte-identical; another Server 2022 ISO will not
reproduce the goldens, because `install.wim` is a different size and therefore lands in
different clusters.

## Client media

| Release | Channel | `install.wim` | BUILD | Images | `ei.cfg` | Level |
|---|---|---|---|---|---|---|
| Windows 11 22H2 | Retail | 5097462333 | 22621 | 11: Home/Pro/Education families | absent | Built |
| Windows 10 22H2 | Eval | 4594586062 | 19041 | 1: Enterprise Evaluation | present | **Installed** |

Both report `ARCH 9` and `PRODUCTTYPE WinNT`, with every image `INSTALLATIONTYPE Client`.

Client media carries far more editions than server media, and the choice matters:
**Windows 10 and 11 Home have no RDP host and cannot domain-join.** Home is a working
target over SSH and the serial console, but selecting it with RDP enabled will not give you
Remote Desktop.

## Media that will not work

| Release | Why |
|---|---|
| Windows 11 25H2 Arm64 (`install.wim` 7075454641, BUILD 26200) | `ARCH 12`. The drivers are amd64 and the answer file hardcodes `processorArchitecture="amd64"`. |

Kept on hand deliberately: it is the only media available that exercises the architecture
refusal, so it is a test fixture rather than a gap. The refusal is enforced in
`builder::assemble`, so it covers the GUI and the CLI alike — building from this ISO fails
with a message naming Arm64 and leaves no image behind.

## Hardware verification status

| Release | Status |
|---|---|
| Server 2019 | **Installed and verified** |
| Server 2022 | **Installed and verified** (the reference ISO) |
| Server 2022 volume licensing | **Installed and verified** — proves the `_Default` licence channel |
| Windows 10 22H2 | **Installed and verified** |
| Server 2025 | Blocked on [propolis#1199](https://github.com/oxidecomputer/propolis/pull/1199) |
| Windows 11 22H2 | Blocked on [propolis#1199](https://github.com/oxidecomputer/propolis/pull/1199) |

Server 2025 and Windows 11 could not see any disk on Propolis: two defects in its NVMe
emulation, unrelated to anything this builder produces. The same images install on KVM and
under QEMU. See [NVME-SERVER2025-INVESTIGATION.md](NVME-SERVER2025-INVESTIGATION.md).

**The four verified installs predate two changes**, so they are not verification of the
current tree: `pvpanic` and `viosock` were added to the driver payload (which changes the
media contents of every image), and the answer file gained the `UpgradeData` element. Both
are believed harmless — the drivers only remove two "unknown device" entries, and
`UpgradeData` asserts what was already true for those releases — but neither has been
re-tested on hardware. Re-verify at least Server 2022 before treating the current tree as
proven.

## Images built for hardware verification

Built 2026-08-19 and uploaded to the `danb` project on rack2, one per release, awaiting
installation. Administrator `Oxide`; server images are Datacenter with Desktop Experience.

| Disk on rack2 | Media | Edition |
|---|---|---|
| `ws2019-datacenter-installer` | Server 2019 eval | Datacenter Desktop |
| `ws2022-datacenter-installer` | Server 2022 eval (the reference ISO) | Datacenter Desktop |
| `ws2022-datacenter-vl-installer` | Server 2022 volume licensing | Datacenter Desktop |
| `ws2025-datacenter-installer` | Server 2025 eval | Datacenter Desktop |
| `win11-pro-installer` | Windows 11 22H2 retail | Professional |
| `win10-enterprise-installer` | Windows 10 22H2 eval | Enterprise Evaluation |

Windows 10 is Enterprise rather than Pro because that ISO carries exactly one image; there
is no Pro on it to choose.

Each image was checked before upload rather than assumed: the exFAT volume mounted, and the
answer file, the `ei.cfg`, the bootstrap script and the driver set read back out of it. That
check earned its keep — it caught Server 2025 and Windows 11 carrying Server 2022 drivers,
because the driver lookup was reading the caller's asserted release instead of the one
detected from the media.

## Adding a row

Record the four identity fields and the level actually reached — not the level intended.
If an install was verified, say what was checked; "it booted" is not a level.

The reference-media row above exists because someone hardcoded a WIM size in a test years
ago and it happened to still be traceable. That was luck. **Record the ISO whenever media
is used for a verified install,** so the next person does not have to reconstruct it from
a constant in a golden.
