# Third-party software

This workspace is Copyright 2026 Oxide Computer Company, licensed **MPL-2.0** (see
`LICENSE`). The *image* it builds is mostly not ours. `tools/fetch-payload.sh` downloads
five third-party components against pinned versions and SHA-256 checksums, `build.rs`
embeds them with `include_bytes!`, and `builder::assemble` writes them onto the media
unmodified.

Two of them are copyleft, so **every release of this tool conveys GPL'd object code** and
owes notices and corresponding source. That is a distribution obligation, not a code
one — nothing here is a derivative work of them.

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

The GPL'd components are standalone UEFI executables, conveyed byte-for-byte as
downloaded and executed by firmware in its own context. Nothing in `oxwin-core` links
against them, calls into them, or shares data structures with them; `builder::assemble`
copies bytes onto a partition. That is mere aggregation under GPLv2 §2 and GPLv3 §5, and
the workspace remains MPL-2.0.
