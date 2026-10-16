#!/usr/bin/env bash
# Boot out/disk.raw under QEMU/KVM with OVMF, as a sanity check of the
# firmware -> EFI stub -> kernel -> /init path. This is NOT a TDX guest: the
# TDX checks of /init (RTMRs, CCEL, quotes) are expected to fail here, and the
# kernel has no driver for QEMU's NIC (gVNIC only), so there is no network.
#
#   scripts/qemu-boot.sh [--disk PATH] [--timeout SECONDS] [--until STRING] [--log PATH]
#
#   --disk     disk image (default out/disk.raw); attached snapshot=on, never modified
#   --timeout  kill QEMU after this many seconds (default 180)
#   --until    stop as soon as the serial log contains STRING (e.g. "PROBE DONE")
#   --log      serial log (default out/qemu-serial.log)
#
# Exit status: 0 if --until matched, or (without --until) QEMU exited by itself.
set -euo pipefail

QEMU_IMAGE=vopenshell/qemu:ef374acdb6e018eb

die() { echo "qemu-boot: $*" >&2; exit 1; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
disk="$repo/out/disk.raw"
log="$repo/out/qemu-serial.log"
timeout_s=180
until_str=
while [[ $# -gt 0 ]]; do
	case "$1" in
		--disk) disk="${2:?}"; shift 2 ;;
		--timeout) timeout_s="${2:?}"; shift 2 ;;
		--until) until_str="${2:?}"; shift 2 ;;
		--log) log="${2:?}"; shift 2 ;;
		-h | --help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
		*) die "unknown argument: $1" ;;
	esac
done
[[ "$timeout_s" =~ ^[0-9]+$ ]] || die "--timeout must be a number of seconds"
[[ -f "$disk" ]] || die "missing $disk (run scripts/build-image.sh)"
[[ -r /dev/kvm && -w /dev/kvm ]] || die "/dev/kvm is not accessible"
disk="$(realpath "$disk")"
mkdir -p "$(dirname "$log")"
log="$(realpath "$log")"
: >"$log"

echo "qemu-boot: booting $disk (timeout ${timeout_s}s); serial log: $log" >&2
set +e
docker run --rm -u "$(id -u):$(id -g)" --group-add "$(stat -c %g /dev/kvm)" --device /dev/kvm \
	-v "$disk:/img/disk.raw:ro" -v "$log:/log/serial.log" \
	-e TIMEOUT="$timeout_s" -e UNTIL="$until_str" --entrypoint bash "$QEMU_IMAGE" -c '
qemu-system-x86_64 -machine q35 -accel kvm -cpu host -m 2048 -smp 2 \
	-bios /usr/share/qemu/OVMF.fd \
	-drive file=/img/disk.raw,format=raw,if=none,id=boot,snapshot=on \
	-device nvme,drive=boot,serial=vcontainer \
	-nic user,model=virtio-net-pci \
	-nographic -monitor none -serial file:/log/serial.log -no-reboot &
pid=$!
rc=1
for _ in $(seq "$TIMEOUT"); do
	if ! kill -0 "$pid" 2>/dev/null; then [ -n "$UNTIL" ] || rc=0; break; fi
	if [ -n "$UNTIL" ] && grep -qF "$UNTIL" /log/serial.log; then rc=0; sleep 1; break; fi
	sleep 1
done
kill "$pid" 2>/dev/null
wait "$pid" 2>/dev/null
exit "$rc"
'
rc=$?
set -e
echo "qemu-boot: exit $rc; serial log: $log"
exit "$rc"
