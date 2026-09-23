# Third-party software

This workspace is Copyright 2026 Oxide Computer Company, licensed **MPL-2.0** (see
`LICENSE`). The *image* it builds is mostly not ours. `tools/fetch-payload.sh` downloads
six third-party components against pinned versions and SHA-256 checksums, `build.rs`
embeds them with `include_bytes!`, and `builder::assemble` writes them onto the media
unmodified. Two more — a CPython runtime and mtools — arrive bundled *inside* one of
those six (the cloudbase-init MSI) rather than being fetched and pinned separately; their
versions are read off the binaries themselves.

Three of the eight are copyleft, so **every release of this tool conveys GPL'd object
code** and owes notices and corresponding source. That is a distribution obligation, not
a code one — nothing here is a derivative work of them.

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

The cloudbase-init MSI is upstream's stable, EV code-signed release, not the 2022 Oxide
fork the public docs point at — both of that fork's functional patches have since landed
upstream, and this one is versioned, pinnable and checked (`bootstrap.ps1` verifies its
Authenticode signature before installing it). CPython's version and mtools' presence and
version were both read off the installed binaries: `python3.dll` embeds a `tags/v3.13.13`
build tag, and `mcopy.exe` embeds the string `mtools-4.0.18`. mtools is a third GPL'd
component this image conveys — brought in by cloudbase-init's installer, not chosen by
us — so it is listed here with the same obligations as the other two.

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
downloaded and executed by firmware in its own context. mtools is conveyed the same
way at one remove: it arrives inside the cloudbase-init MSI, which `builder::assemble`
copies onto the media unmodified, and it runs as a separate Windows process the guest's
`bootstrap.ps1` never links against or calls into. In every case, nothing in `oxwin-core`
links against, calls into, or shares data structures with the GPL'd binary; this
workspace only ever copies bytes onto a partition. That is mere aggregation under
GPLv2 §2 and GPLv3 §5, and the workspace remains MPL-2.0.
