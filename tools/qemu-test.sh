#!/bin/bash
#
# Boot a built installer image locally, as close to an Oxide instance as QEMU gets.
#
#   ./tools/qemu-test.sh ~/Desktop/oxwin-images/ws2025-verbose.img
#   ./tools/qemu-test.sh --vnc ws2025-verbose.img     # and watch the screen
#
# Prints the guest's serial console to stdout, which for our media is the only view there
# is: Windows Setup renders to a framebuffer, and an Oxide instance has none. What you are
# watching for is the `OXIDE-STAGE` markers the answer file echoes to COM1.
#
# What this deliberately mimics, and why:
#
#   * UEFI with no Secure Boot and **no TPM**. That is the Oxide condition, and it is the
#     whole reason the answer file carries the Windows 11 LabConfig bypasses.
#   * NVMe disks, not virtio-blk. Oxide guest disks are NVMe — which is why viostor and
#     vioscsi are only carried as insurance.
#   * virtio-net for the NIC, so NetKVM is the driver under test.
#   * No display device at all, matching a rack guest.
#   * The installer as disk 0 and a blank disk as disk 1, because the answer file installs
#     to disk 1 by default. Getting that order wrong makes Setup overwrite the installer.
#
# What it cannot tell you, from CLAUDE.md: a QEMU guest honours its own NVRAM, so after an
# install it re-enters the installed OS directly and never exercises the chooser's
# "installed Windows exists" branch. There is no VPC firewall either, so RDP appears to
# work here and then times out on a rack.
set -euo pipefail

VNC=0
ARGS=()
for a in "$@"; do
  case "$a" in
    --vnc) VNC=1 ;;
    *) ARGS+=("$a") ;;
  esac
done
IMAGE="${ARGS[0]:-}"
if [[ -z "$IMAGE" || ! -f "$IMAGE" ]]; then
  echo "usage: $0 [--vnc] <installer.img> [target-disk-gib]" >&2
  exit 2
fi
TARGET_GIB="${ARGS[1]:-60}"

# Oxide imports the *target* disk at 4096 bytes and the installer at 512. Matching that
# matters: a 4Kn disk changes how Setup partitions and aligns, and "it worked locally"
# means nothing if the local disk had a different geometry from the rack's.
TARGET_BS="${OXWIN_QEMU_TARGET_BS:-512}"
MEM="${OXWIN_QEMU_MEM:-8192}"

# --vnc adds a display so Windows Setup can be watched, which is the one thing the serial
# console cannot show: Setup renders to a framebuffer and says nothing on COM1 between our
# own markers. Note it makes the guest *less* like an Oxide instance, which has no display
# adapter at all — so it is a diagnostic, not the configuration to certify against.
VNC_PORT="${OXWIN_QEMU_VNC:-1}"
if (( VNC )); then
  DISPLAY_ARGS=(-vga std -display none -vnc 127.0.0.1:"$VNC_PORT")
else
  DISPLAY_ARGS=(-vga none -display none)
fi

FW_CODE=/opt/homebrew/share/qemu/edk2-x86_64-code.fd
FW_VARS_TEMPLATE=/opt/homebrew/share/qemu/edk2-i386-vars.fd
for f in "$FW_CODE" "$FW_VARS_TEMPLATE"; do
  [[ -f "$f" ]] || { echo "missing UEFI firmware: $f (brew install qemu)" >&2; exit 1; }
done

run="${TMPDIR:-/tmp}/oxwin-qemu-$(basename "${IMAGE%.img}")-bs${TARGET_BS}-${TARGET_GIB}g"
mkdir -p "$run"
vars="$run/uefi-vars.fd"
target="$run/target.raw"
# Fresh NVRAM every run: stale UEFI boot entries are exactly how a "it booted the wrong thing"
# hour gets spent.
cp "$FW_VARS_TEMPLATE" "$vars"
# Sparse, so a 60 GiB blank disk costs nothing until Setup writes to it.
[[ -f "$target" ]] || qemu-img create -f raw "$target" "${TARGET_GIB}G" >/dev/null

# TCG only on Apple silicon — there is no hardware acceleration for an amd64 guest on an
# arm64 host. QEMU 11's TCG is multi-threaded, so vCPUs genuinely help; leave the host
# some cores rather than taking all of them.
host_cores="$(sysctl -n hw.ncpu)"
vcpus="${OXWIN_QEMU_CPUS:-$(( host_cores > 10 ? 8 : 4 ))}"

echo "image:   $IMAGE"
echo "target:  $target (${TARGET_GIB} GiB sparse)"
echo "vcpus:   $vcpus (tcg, no hardware acceleration on arm64), ${MEM} MiB"
echo "target bs: $TARGET_BS (Oxide uses 4096 for the target disk)"
echo "scratch: $run"
if (( VNC )); then
  echo "vnc:     open vnc://127.0.0.1:$((5900 + VNC_PORT))   (Screen Sharing, or any VNC client)"
fi
echo "--- serial console follows; ctrl-a x quits ---"

exec qemu-system-x86_64 \
  -machine q35 \
  -cpu max \
  -smp "$vcpus" \
  -m "$MEM" \
  -rtc base=utc \
  -drive if=pflash,format=raw,unit=0,readonly=on,file="$FW_CODE" \
  -drive if=pflash,format=raw,unit=1,file="$vars" \
  -drive file="$IMAGE",if=none,id=installer,format=raw,snapshot=on \
  -device nvme,drive=installer,serial=installer \
  -drive file="$target",if=none,id=target,format=raw \
  -device nvme,drive=target,serial=target,logical_block_size="$TARGET_BS",physical_block_size="$TARGET_BS" \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  "${DISPLAY_ARGS[@]}" \
  -serial mon:stdio
