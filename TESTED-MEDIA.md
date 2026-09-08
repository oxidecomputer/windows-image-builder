# Tested media

Which Windows ISOs this has actually been run against, so the answer to "I have
`en-us-random-server-20xx.iso`, will it work?" is a lookup rather than a guess.

**Do not read a blank row as "broken".** Most media will work; the table records what has
been *demonstrated*, and the honest answer for anything absent is "nobody has tried it".

## How to identify your media

Filenames are unreliable: they are chosen by whoever downloaded the ISO, and Microsoft
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
| **Installed** | Booted on an Oxide rack and installed, artifact checked: `C:\oxide-bootstrap.log` present, NIC bound, intended edition |

## Server media

| Release | Channel | `install.wim` | BUILD | Editions | `ei.cfg` | Level |
|---|---|---|---|---|---|---|
| Server 2016 | Eval | 4373893584 | 14393 | `ServerStandardEval`, `ServerDatacenterEval` | present | Built |
| Server 2019 | Eval | 4309326283 | 17763 | `ServerStandardEval`, `ServerDatacenterEval` | present | **Installed** |
| Server 2022 | Eval | **4340202461** | 20348 | `ServerStandardEval`, `ServerDatacenterEval` | present | **Installed** |
| Server 2022 | Volume licensing | 4857802167 | 20348 | `ServerStandard`, `ServerDatacenter` | absent | **Installed** |
| Server 2025 | Eval | 5189163060 | 26100 | `ServerStandardEval`, `ServerDatacenterEval` | present | Built |

All server media reports `ARCH 9` (amd64) and `PRODUCTTYPE ServerNT`, and carries four
images: Standard and Datacenter, each as `Server` and `Server Core`. Server 2016 uses the
same edition vocabulary as 2019, 2022 and 2025, including the
`ServerDatacenterEval`/`ServerDataCenterEvalCore` casing split between `EDITIONID` and
`FLAGS`. That is why `unattend::server_editions` is one table parameterised by year rather
than four.

### Server 2016 and NVMe

**Server 2016 builds and installs correctly, and it is the one release with a known risk
on an Oxide rack.** The risk is not in anything this builder produces.

`stornvme` on 14393 discovers namespaces by issuing `Identify` with `CNS 00h` for every
namespace ID from 1 up to the controller's advertised `NN`, rather than asking for the
Active Namespace ID List (`CNS 02h`). Server 2019 and later discard the zero-capacity
replies; **2016 creates a disk object for every one of them.** Against a controller that
advertises a large `NN`, Setup is left enumerating hundreds of devices and never reaches
the answer file.

Seen under QEMU, whose `nvme` device reports `NN = 256` regardless of how many namespaces
are backed (`NVME_MAX_NAMESPACES`, compile-time, with no device property to change it).
Two controllers produced **509 zero-byte disks**, all attributed to `QEMU NVMe Ctrl`, with
the real installer at index 0 and the real target at index 255. The tell is that
`setuperr.log` is **blank** — Setup is not failing, it is grinding — so from the outside
this is indistinguishable from a slow install, which on a rack guest with no framebuffer
is indistinguishable from a hang. The same image installed end to end, through to the SAC
prompt, when the disks were moved to AHCI.

Anyone who needs Server 2016 on Oxide should open an issue. Server 2016 goes out of support
in January 2027, unless someone has a strong need, we will leave 2016 as un-supported.

Everything else about 2016 is settled by reading and by the build: detection, the same
edition vocabulary as the later server releases, the `Eval` licence channel resolving to a
directory that really exists in the image, and virtio-win 0.1.285's `2k16` drivers, which
are byte-identical to `2k19` — 38 files with matching hashes.

Server 2022 EVAL `install.wim` of 4340202461 bytes is hardcoded in `exfat.rs` and
 `testdata/exfat/placements.json`, so every committed golden: and the image that installed
on a rack first try: came from that specific ISO. It is the one to use when checking that
a change leaves the output byte-identical; another Server 2022 ISO will not reproduce the
goldens, because `install.wim` is a different size and therefore lands in different clusters.

## Client media

| Release | Channel | `install.wim` | BUILD | Images | `ei.cfg` | Level |
|---|---|---|---|---|---|---|
| Windows 11 22H2 | Retail | 5097462333 | 22621 | 11: Home/Pro/Education families | absent | Built |
| Windows 10 22H2 | Eval | 4594586062 | 19041 | 1: Enterprise Evaluation | present | **Installed** |

Both report `ARCH 9` (AMD64) and `PRODUCTTYPE WinNT`, with every image `INSTALLATIONTYPE Client`.

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
| Server 2016 | Built and installed under emulation, never on a rack — see [Server 2016 and NVMe](#server-2016-and-nvme) |
| Server 2019 | **Installed and verified** |
| Server 2022 | **Installed and verified** (the reference ISO) |
| Server 2022 volume licensing | **Installed and verified** — proves the `_Default` licence channel |
| Windows 10 22H2 | **Installed and verified** |
| Server 2025 | Blocked, on two separate things — see below |
| Windows 11 22H2 | Blocked, on two separate things — see below |

Server 2025 and Windows 11 are held up by **a Propolis NVMe problem**:

**Propolis NVMe emulation.** The way device registers work right now for NVMe causes
newer Windows kernels to panic and keep restarting the virtual device. An early attempt
has been made to fix the problem, [propolis#1199](https://github.com/oxidecomputer/propolis/pull/1199).
The Propolis team is working on a better long term fix!

## Golden images

A golden build does more than randomise the computer name: once the install finishes the
guest generalizes itself with `sysprep /generalize /oobe /shutdown` and powers off, ready
to snapshot. Verified on rack2 with Server 2022 evaluation media on 2026-08-31: the whole
cycle — upload, install, generalize, shutdown, snapshot, image, teardown — ran end to end
under `oxwin golden`, port 22 answering at 4m52s and the instance stopped at 6m20s.

Only Server 2019 and 2022 evaluation media has been through a golden cycle. The other verified rows
above are ordinary installs.
