# Third-party software

This workspace is Copyright 2026 Oxide Computer Company, licensed **MPL-2.0** (see
`LICENSE`). The *image* it builds is mostly not ours. `tools/fetch-payload.sh` downloads
six third-party components against pinned versions and SHA-256 checksums, `build.rs`
embeds them with `include_bytes!`, and `builder::assemble` writes them onto the media
unmodified. Four more — a CPython runtime, mtools, bsdtar and Elevate.exe — arrive
bundled *inside* one of those six (the cloudbase-init MSI) rather than being fetched and
pinned separately; their versions and, for Elevate.exe, its very identity, are read off
the binaries themselves rather than guessed.

Four of the ten are copyleft (two GPL, one LGPL), so **every release of this tool
conveys GPL'd and LGPL'd object code** and owes notices and corresponding source. That
is a distribution obligation, not a code one — nothing here is a derivative work of them.

The authoritative table is `crates/oxwin-core/src/notices.rs`. One test cross-checks its
versions against `tools/fetch-payload.sh`, because that is the drift that would otherwise
be silent; everything else here is maintained by hand and reviewed when the payload
changes. `oxwin licenses --full` prints all of it from the binary, with no network.

## What we ship

| Component | Version | License | Carried as |
|---|---|---|---|
| [uefi-ntfs](https://github.com/pbatard/uefi-ntfs) | v2.8 | **GPL-2.0-or-later** | `EFI/Boot/bootx64.efi` on the ESP |
| [efifs](https://github.com/pbatard/efifs) | v1.12 | **GPL-3.0-or-later** | `EFI/Rufus/exfat_x64.efi`, `EFI/Boot/udf_x64.efi`, `EFI/Boot/iso9660_x64.efi` |
| [UEFI-Shell](https://github.com/pbatard/UEFI-Shell) | 26H1 | BSD-2-Clause-Patent | `EFI/Shell/shellx64.efi` |
| [virtio-win guest drivers](https://github.com/virtio-win/kvm-guest-drivers-windows) | 0.1.285-1 | BSD-3-Clause | `\drivers\` on the exFAT volume |
| [Win32-OpenSSH](https://github.com/PowerShell/Win32-OpenSSH) | 10.0.0.0p2-Preview | OpenSSH (BSD-style) | `\OpenSSH-Win64.zip` on the exFAT volume |
| [cloudbase-init](https://github.com/cloudbase/cloudbase-init) | 1.1.8 | Apache-2.0 | `\cloudbase\CloudbaseInitSetup_x64.msi` on the exFAT volume, installed by the bootstrap script |
| [CPython](https://www.python.org/) | 3.13.13 | PSF (Python-2.0.1) | bundled inside the cloudbase-init MSI; installed to `Cloudbase-Init\Python` |
| [mtools](https://www.gnu.org/software/mtools/) (`mcopy.exe`, `mdir.exe`) | 4.0.18 | **GPL-3.0-or-later** | bundled inside the cloudbase-init MSI, installed alongside cloudbase-init's own executables |
| [libarchive](https://www.libarchive.org/) (`bsdtar.exe`) | 3.1.2 | BSD-2-Clause | bundled inside the cloudbase-init MSI, used to extract the config drive's ISO |
| [jpassing/elevate](https://github.com/jpassing/elevate) (`Elevate.exe`, identity inferred — see below) | unversioned | **LGPL-2.1-or-later** (disputed — see below) | bundled inside the cloudbase-init MSI, used internally for privilege elevation |

The cloudbase-init MSI is upstream's stable, EV code-signed release, not the 2022 Oxide
fork the public docs point at — both of that fork's functional patches have since landed
upstream, and this one is versioned, pinnable and checked (`bootstrap.ps1` verifies its
Authenticode signature before installing it). CPython's version, mtools' presence and
version, and bsdtar's version were all read off the installed binaries: `python3.dll`
embeds a `tags/v3.13.13` build tag, `mcopy.exe` embeds the string `mtools-4.0.18`, and
`bsdtar.exe` embeds the string `libarchive 3.1.2`. mtools and bsdtar arrive with their
own license files (`mtools.COPYING`, `bsdtar.COPYING`) sitting next to them in the MSI.
mtools is a third GPL'd component this image conveys — brought in by cloudbase-init's
installer, not chosen by us — so it is listed here with the same obligations as
uefi-ntfs and efifs.

### Elevate.exe: identified, and its own upstream disagrees with itself

`Elevate.exe` ships in the MSI with **no adjacent license file at all**, unlike every
other bundled binary above. Its own `VERSIONINFO` resource is entirely blank — no
`CompanyName`, `ProductName`, `LegalCopyright` or `OriginalFilename` — so nothing about
the binary announces what it is.

It was identified rather than assumed, from three independent matches against
[jpassing/elevate](https://github.com/jpassing/elevate), Johannes Passing's small
`ShellExecuteEx`-based UAC launcher:

- the PDB path baked into the binary, `C:\Temp\elevate\bin\x64\Release\Elevate.pdb`;
- its import table, which names only `KERNEL32.dll` and `SHELL32.dll` — consistent with
  a single-purpose `ShellExecuteEx` wrapper and nothing else; and
- the exact shape of that call, `SEE_MASK_FLAG_NO_UI | SEE_MASK_NOCLOSEPROCESS`, which
  matches `Elevate/main.c` in that repository line for line.

That repository is not internally consistent about its own terms. `Elevate/main.c`'s own
header states: "This library is free software; you can redistribute it and/or modify it
under the terms of the GNU Lesser General Public License ... version 2.1 of the License,
or (at your option) any later version", copyright Johannes Passing, 2007. The
repository's separate top-level `LICENSE.md`, by contrast, says MIT — but with the
template placeholder `Copyright (c) <year> <copyright holders>` never filled in, which
reads as a `LICENSE.md` GitHub added to the repo later rather than a relicense the
author actually signed off on.

Per this document's own rule for `bsdtar` — "the actual statements in the files are
controlling" — the file header is treated as authoritative here, and `Elevate.exe` is
listed as **LGPL-2.1-or-later**, not MIT. Over-complying by conveying a notice and
source we might not strictly owe costs nothing; under-complying if LGPL actually governs
would not. A reader who disagrees with that reading has both statements above to redo
the judgement. No commit or tag of `jpassing/elevate` could be tied to the exact object
code cloudbase-init built, since the binary carries no version at all, so the source URL
in `notices.rs` pins the latest commit on that repository's default branch as of this
review (2026-09-23) rather than an unpinned `HEAD`.

Full texts are committed under `licenses/`. That is the one exception to the ground rule
that third-party files are not committed: the rule exists to keep *binaries* out of the
tree, and a license text is exactly the thing that has to travel with the artifact rather
than be fetched by whoever builds it.

### Rufus is not in here

The `\EFI\Rufus\exfat_x64.efi` path on the ESP is a hardcoded lookup path baked into
uefi-ntfs, which never embeds its filesystem driver and loads one from there at runtime.
No Rufus code or binary is fetched, embedded or shipped. Rufus and uefi-ntfs share an
author, which is where the name comes from.

### Why the copyleft components cannot simply be dropped

Oxide's firmware has no UDF or exFAT filesystem driver, so it cannot read a Windows ISO's
only real volume — that is the entire reason the boot shim exists. There is no permissive
substitute: efifs is GPL precisely *because* it is a repackaging of GRUB 2.0's read-only
drivers, and EDK2 ships a FAT driver and nothing else.

## Scope: why the MPL code stays MPL

uefi-ntfs and efifs are standalone UEFI executables, conveyed byte-for-byte as
downloaded and executed by firmware in its own context. mtools and Elevate.exe are
conveyed the same way at one remove: both arrive inside the cloudbase-init MSI, which
`builder::assemble` copies onto the media unmodified, and both run as separate Windows
processes the guest's `bootstrap.ps1` never links against or calls into. In every case,
nothing in `oxwin-core` links against, calls into, or shares data structures with the
GPL'd or LGPL'd binary; this workspace only ever copies bytes onto a partition. That is
mere aggregation under GPLv2 §2, GPLv3 §5 and LGPLv2.1 §5's own "work that uses the
library" test, and the workspace remains MPL-2.0.
