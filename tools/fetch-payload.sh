#!/usr/bin/env bash
#
# Fetches the third-party binaries the answer disk carries: virtio guest drivers
# and Win32-OpenSSH. Nothing here is committed to git — CI runs this before
# packaging, and developers run it once by hand.
#
#   ./tools/fetch-payload.sh [--cache DIR]
#
# Writes to assets/ at the repository root, which is gitignored. The build embeds
# whatever is there; nothing works until this has been run once.
#
# Versions are pinned and checksummed rather than tracking "latest", so a build is
# reproducible and an upstream compromise cannot silently change what ships in a
# guest image. Bumping a version means bumping the checksum next to it.
#
# Requires: curl, bsdtar (libarchive; on Debian/Ubuntu CI: apt-get install
# libarchive-tools), shasum or sha256sum.

set -euo pipefail

VIRTIO_VERSION="0.1.285-1"
VIRTIO_SHA256="e14cf2b94492c3e925f0070ba7fdfedeb2048c91eea9c5a5afb30232a3976331"
VIRTIO_URL="https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-${VIRTIO_VERSION}/virtio-win.iso"

OPENSSH_VERSION="10.0.0.0p2-Preview"
OPENSSH_SHA256="23f50f3458c4c5d0b12217c6a5ddfde0137210a30fa870e98b29827f7b43aba5"
OPENSSH_URL="https://github.com/PowerShell/Win32-OpenSSH/releases/download/${OPENSSH_VERSION}/OpenSSH-Win64.zip"

# UEFI boot shim. Oxide's firmware has no UDF (or ISO9660) filesystem driver, so it
# cannot read the main volume of a Windows ISO — see "Firmware constraints" in the
# README. We ship the missing drivers and a UEFI Shell to load them, which lets the
# stock ISO stay generic instead of being rebuilt.
EFIFS_VERSION="v1.12"
EFIFS_UDF_SHA256="bf09c7808d9585ac03f0f61d8e5e08065ce4f2ada26d4c2671a043f3a7fe65a6"
# UEFI:NTFS loads its filesystem driver from \EFI\Rufus\exfat_x64.efi at runtime.
# efifs is where Rufus gets it, and v1.12 is the version the rack reports loading.
EFIFS_EXFAT_SHA256="21a5969dcd7b6c149b1dc9408c591749ba9c62fb264e2852cc70061fe3defff6"
EFIFS_ISO9660_SHA256="f96b897f49aa43fdbbc6162d7e252acff36feeaad3ca798956a0cf90ede471b0"
EFIFS_URL="https://github.com/pbatard/efifs/releases/download/${EFIFS_VERSION}"

UEFI_SHELL_VERSION="26H1"
UEFI_SHELL_SHA256="4ea080ddd576117cd04f5c02d16712ea5d9249c0752214d8e4055e460d7b11e0"
UEFI_SHELL_URL="https://github.com/pbatard/UEFI-Shell/releases/download/${UEFI_SHELL_VERSION}/shellx64.efi"

# UEFI:NTFS — chainloads Windows Boot Manager off the exFAT volume. The boot partition
# we build carries this plus the UEFI Shell and a startup.nsh chooser.
#
# UEFI:NTFS never embeds its filesystem driver — it loads one at runtime from
# \EFI\Rufus\exfat_x64.efi and fails with "[14] Not Found" if it is absent. Rufus's
# uefi-ntfs.img simply ships that driver alongside the loader in its EFI/Rufus
# directory; the standalone binary we fetch here is the same loader without the
# packaging. Either way the driver has to be placed on the boot partition, and we
# fetch it from efifs below — so this stays a plain pinned download with no image
# extraction step, which is what CI wants.
UEFI_NTFS_VERSION="v2.8"
UEFI_NTFS_SHA256="f04f33833951e7d065a87f0745557436e1e7587f9506576286548c539254bc3c"
UEFI_NTFS_URL="https://github.com/pbatard/uefi-ntfs/releases/download/${UEFI_NTFS_VERSION}/bootx64.efi"

# virtio-win's directory name for each Windows release we support. Every one of these
# must exist in the ISO or this script fails — see the loop below for why.
TARGETS=(2k19 2k22 2k25 w10 w11)

# Drivers worth carrying. NetKVM is the one that actually matters (Oxide NICs are
# virtio-net); viostor/vioscsi are insurance in case guest disks are not NVMe, and
# the rest are small quality-of-life devices.
#
# pvpanic and viosock are here because a guest reported exactly two unknown devices in
# Device Manager — "QEMU PVPanic Device" and "VirtIO Socket Driver" — so those are the
# two virtio devices an Oxide instance exposes that we were not carrying a driver for.
# The list is deliberately the devices observed on real hardware rather than everything
# the virtio ISO offers: it also ships qxl, viogpudo, vioinput, viomem, viofs, smbus and
# more, and there is no evidence any of those devices exist on an Oxide sled.
#
# viosock brings two `*-test.exe` tools along with it. They are left in: the payload
# filter is an exclusion of `*.pdb` and nothing else, on purpose, because filtering by
# extension is what silently dropped netkvmp.exe once and left guests with no network.
DRIVERS=(NetKVM viostor vioscsi Balloon vioserial viorng pvpanic viosock)

# Drivers whose absence is a hard failure rather than a note. A guest with no NetKVM
# has no network, which is indistinguishable from a dozen other problems and expensive
# to diagnose after the install.
REQUIRED_DRIVERS=(NetKVM)

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cache="${repo_root}/.cache"
assets="${repo_root}/assets"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cache) cache="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

fetch() {
  local url="$1" dest="$2" want="$3"
  if [[ -f "$dest" ]] && [[ "$(sha256 "$dest")" == "$want" ]]; then
    echo "  cached  $(basename "$dest")"
    return
  fi
  echo "  get     $(basename "$dest")"
  curl -fsSL --retry 3 -o "${dest}.part" "$url"
  local got
  got="$(sha256 "${dest}.part")"
  if [[ "$got" != "$want" ]]; then
    rm -f "${dest}.part"
    echo "checksum mismatch for ${url}" >&2
    echo "  expected ${want}" >&2
    echo "  actual   ${got}" >&2
    exit 1
  fi
  mv "${dest}.part" "$dest"
}

mkdir -p "$cache"

echo "virtio-win ${VIRTIO_VERSION}"
fetch "$VIRTIO_URL" "${cache}/virtio-win-${VIRTIO_VERSION}.iso" "$VIRTIO_SHA256"

echo "Win32-OpenSSH ${OPENSSH_VERSION}"
fetch "$OPENSSH_URL" "${cache}/OpenSSH-Win64-${OPENSSH_VERSION}.zip" "$OPENSSH_SHA256"

echo "UEFI boot shim"
fetch "${EFIFS_URL}/udf_x64.efi" "${cache}/udf_x64-${EFIFS_VERSION}.efi" "$EFIFS_UDF_SHA256"
fetch "${EFIFS_URL}/iso9660_x64.efi" "${cache}/iso9660_x64-${EFIFS_VERSION}.efi" "$EFIFS_ISO9660_SHA256"
fetch "${EFIFS_URL}/exfat_x64.efi" "${cache}/exfat_x64-${EFIFS_VERSION}.efi" "$EFIFS_EXFAT_SHA256"
fetch "$UEFI_SHELL_URL" "${cache}/shellx64-${UEFI_SHELL_VERSION}.efi" "$UEFI_SHELL_SHA256"
fetch "$UEFI_NTFS_URL" "${cache}/uefintfs-${UEFI_NTFS_VERSION}.efi" "$UEFI_NTFS_SHA256"

rm -rf "${assets}/drivers" "${assets}/openssh" "${assets}/efi"
mkdir -p "${assets}/openssh" "${assets}/efi"
cp "${cache}/OpenSSH-Win64-${OPENSSH_VERSION}.zip" "${assets}/openssh/OpenSSH-Win64.zip"
cp "${cache}/udf_x64-${EFIFS_VERSION}.efi" "${assets}/efi/udf_x64.efi"
cp "${cache}/iso9660_x64-${EFIFS_VERSION}.efi" "${assets}/efi/iso9660_x64.efi"
cp "${cache}/exfat_x64-${EFIFS_VERSION}.efi" "${assets}/efi/exfat_x64.efi"
cp "${cache}/shellx64-${UEFI_SHELL_VERSION}.efi" "${assets}/efi/shellx64.efi"
cp "${cache}/uefintfs-${UEFI_NTFS_VERSION}.efi" "${assets}/efi/uefintfs.efi"

# Extract straight out of the ISO. Everything except .pdb symbols is kept.
#
# Do not be tempted to drop .exe: netkvm.inf lists netkvmp.exe in [SourceDisksFiles],
# so without it pnputil fails the whole package with "The system cannot find the file
# specified" and the guest comes up with no NIC. An INF's payload is whatever the INF
# says it is, not whatever looks like a driver.
staging="$(mktemp -d)"
# ISO9660 hands back mode 444 files inside mode 555 directories, so the tree has to
# be made writable before it can be removed.
trap 'chmod -R u+w "$staging" 2>/dev/null; rm -rf "$staging"' EXIT

echo "extracting drivers"

# The whole ISO comes out, rather than only the directories we want. Its per-driver
# directories are hard links to canonical copies elsewhere in the image, so a
# partial extract dies on every unresolved link target. A full extract takes well
# under a second and lands in a temp dir that goes away on exit.
bsdtar -xf "${cache}/virtio-win-${VIRTIO_VERSION}.iso" -C "$staging"

# A requested target that this ISO does not carry is a hard failure, not a note.
#
# This used to skip with a log line and fail only if *nothing at all* matched, which
# meant a typo in TARGETS — or an upstream rename — produced a payload with no drivers
# for that release, an image that built without complaint, and a guest with no network.
# Every silent-failure mode in this project looks exactly like that.
#
# An individual driver missing from a target that is otherwise present is still only a
# note: virtio-win genuinely does not ship every device for every release. The ones we
# cannot do without are listed in REQUIRED_DRIVERS.
for target in "${TARGETS[@]}"; do
  target_found=0
  for driver in "${DRIVERS[@]}"; do
    src="${staging}/${driver}/${target}/amd64"
    if [[ ! -d "$src" ]]; then
      for required in "${REQUIRED_DRIVERS[@]}"; do
        if [[ "$driver" == "$required" ]]; then
          echo "virtio-win ${VIRTIO_VERSION} has no ${driver}/${target}/amd64," >&2
          echo "and ${driver} is required: a guest without it has no network." >&2
          exit 1
        fi
      done
      echo "  skip    ${driver}/${target}/amd64 (not in this virtio-win release)"
      continue
    fi
    dest="${assets}/drivers/${target}/${driver}"
    mkdir -p "$dest"
    # Keep everything the INF might reference; only .pdb symbols are excluded.
    find "$src" -type f ! -iname '*.pdb' -exec cp {} "$dest/" \;
    chmod u+w "$dest"/* 2>/dev/null || true
    echo "  ok      ${driver}/${target}/amd64 -> $(ls -1 "$dest" | wc -l | tr -d ' ') files"
    target_found=$((target_found + 1))
  done
  if [[ $target_found -eq 0 ]]; then
    echo "virtio-win ${VIRTIO_VERSION} carries no drivers at all for target ${target}." >&2
    echo "Either the target name is wrong or the ISO layout changed; both need a human." >&2
    exit 1
  fi
done

# The manifest records which drivers belong to which Windows release, and the size of
# each file. The build embeds every file under assets/ by walking the tree, and
# cross-checks what it found against this — so a half-finished download is a build
# error rather than an image missing a driver.
echo "writing manifest"
python3 - "$assets" "$VIRTIO_VERSION" "$OPENSSH_VERSION" <<'PY'
import json, os, sys

assets, virtio_version, openssh_version = sys.argv[1:4]
manifest = {
    "virtioWinVersion": virtio_version,
    "openSshVersion": openssh_version,
    "openSsh": "assets/openssh/OpenSSH-Win64.zip",
    "efi": {
        "shell": "assets/efi/shellx64.efi",
        "udf": "assets/efi/udf_x64.efi",
        "iso9660": "assets/efi/iso9660_x64.efi",
    },
    "drivers": {},
}

drivers_root = os.path.join(assets, "drivers")
for target in sorted(os.listdir(drivers_root)):
    entries = []
    target_dir = os.path.join(drivers_root, target)
    for driver in sorted(os.listdir(target_dir)):
        for name in sorted(os.listdir(os.path.join(target_dir, driver))):
            entries.append({
                "driver": driver,
                "name": name,
                "path": f"assets/drivers/{target}/{driver}/{name}",
                "size": os.path.getsize(os.path.join(target_dir, driver, name)),
            })
    manifest["drivers"][target] = entries

out = os.path.join(assets, "payload-manifest.json")
with open(out, "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")

total = sum(e["size"] for v in manifest["drivers"].values() for e in v)
print(f"  {sum(len(v) for v in manifest['drivers'].values())} driver files, {total/1024/1024:.1f} MiB")
PY

echo "payload ready in ${assets}"
