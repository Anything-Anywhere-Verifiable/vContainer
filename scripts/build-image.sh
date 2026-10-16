#!/usr/bin/env bash
# Build the vcontainer boot image: one EFI-stub PE (kernel + built-in
# initramfs + built-in cmdline) on a GPT disk with a single ESP.
#
#   scripts/build-image.sh [--init PATH] [--clean]
#
#   --init PATH  static binary to install as /init
#                (default: target/x86_64-unknown-linux-musl/release/vcontainer-init)
#   --clean      drop the cached kernel objects first (full rebuild)
#
# Everything runs inside the vcontainer/builder:dev container (repo mounted at
# /work, host uid). The kernel source is .cache/linux-6.18.57.tar.xz (sha256
# pinned) plus image/patches/*.patch, extracted to .cache/linux-6.18.57;
# kernel objects are cached in .cache/kbuild-6.18.57.
# Outputs in out/:
#   BOOTX64.EFI          the PE (arch/x86/boot/bzImage)
#   kernel.config        the final kernel .config
#   initramfs.list, init the gen_init_cpio list and the /init it embeds
#   disk.raw             1 GiB GPT disk, ESP only, \EFI\BOOT\BOOTX64.EFI
#   vcontainer.tar.gz    disk.raw packed for `gcloud compute images create`
#   rtmr1.txt            the RTMR1 predicted by tools/predict-rtmr1.py
set -euo pipefail

KVER=6.18.57
KSRC_SHA256=5bd9acdc82ec5efc3790ae840e5aaa40dfde7efff261cf6dde902dc570acf98c
BUILDER=vcontainer/builder:dev
# Fixed identifiers so the GPT (measured into RTMR1) and the FAT are
# byte-for-byte reproducible.
DISK_GUID=7C0E0000-5643-4F4E-5441-494E45520001
ESP_GUID=7C0E0000-5643-4F4E-5441-494E45520002
ESP_NAME=vcontainer-esp
FAT_VOLID=7C0E5643
FAT_LABEL=VCONTAINER
# Used when the tree has no commit yet: 2026-01-01T00:00:00Z.
FALLBACK_EPOCH=1767225600

die() { echo "build-image: $*" >&2; exit 1; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ---------------------------------------------------------------------------
# Inside the builder container.
# ---------------------------------------------------------------------------
if [[ "${1:-}" == --inside ]]; then
	cd /work
	ksrc=/work/.cache/linux-$KVER
	kobj=/work/.cache/kbuild-$KVER
	tarball=/work/.cache/linux-$KVER.tar.xz

	# Source tree = sha256-pinned tarball + image/patches/*.patch. The
	# applied series is recorded in the tree; on any change, re-extract and
	# drop the objects (a re-extracted file keeps its old tarball mtime, so
	# kbuild would not notice a removed patch).
	series="$( (cat image/patches/*.patch 2>/dev/null || true) | sha256sum | cut -c1-64)"
	if [[ "$(cat "$ksrc/.vcontainer-patches" 2>/dev/null)" != "$series" ]]; then
		echo "$KSRC_SHA256  $tarball" | sha256sum -c --quiet - || die "$tarball: sha256 mismatch"
		echo "build-image: extracting $tarball and applying image/patches" >&2
		rm -rf "$ksrc" "$kobj"
		tar -xf "$tarball" -C /work/.cache
		for p in image/patches/*.patch; do
			[[ -e "$p" ]] || continue
			patch -s -d "$ksrc" -p1 --no-backup-if-mismatch <"$p" || die "patch $p failed"
		done
		echo "$series" >"$ksrc/.vcontainer-patches"
	fi
	ts="$(date -u -d "@$SOURCE_DATE_EPOCH" '+%Y-%m-%d %H:%M:%S UTC')"
	export KBUILD_BUILD_TIMESTAMP="$ts" KBUILD_BUILD_USER=vcontainer \
		KBUILD_BUILD_HOST=vcontainer KBUILD_BUILD_VERSION=1
	# Set (even empty) LOCALVERSION stops setlocalversion from probing git
	# and appending "+".
	export LOCALVERSION=
	kmake=(make -C "$ksrc" O="$kobj" -j"$(nproc)")

	# gen_init_cpio list; mtimes come from KBUILD_BUILD_TIMESTAMP (-d).
	cat >out/initramfs.list.new <<-'EOF'
		dir /dev 0755 0 0
		nod /dev/console 0600 0 0 c 5 1
		dir /proc 0755 0 0
		dir /sys 0755 0 0
		dir /run 0755 0 0
		dir /etc 0755 0 0
		dir /newroot 0755 0 0
		file /init /work/out/init 0755 0 0
	EOF
	# Replace only on change, so an unchanged list does not force a relink.
	if cmp -s out/initramfs.list.new out/initramfs.list; then
		rm out/initramfs.list.new
	else
		mv out/initramfs.list.new out/initramfs.list
	fi

	mkdir -p "$kobj"
	"${kmake[@]}" -s allnoconfig KCONFIG_ALLCONFIG=/work/image/kernel.config
	image/check-config.sh "$kobj/.config"
	"${kmake[@]}" bzImage
	image/check-config.sh "$kobj/.config"
	release="$(cat "$kobj/include/config/kernel.release")"
	[[ "$release" == "$KVER-vcontainer" ]] || die "unexpected kernel.release $release"
	cp "$kobj/arch/x86/boot/bzImage" out/BOOTX64.EFI
	cp "$kobj/.config" out/kernel.config

	# Disk: exactly 1 GiB, GPT with one ESP at LBA 2048, FAT32, one file.
	rm -f out/disk.raw
	truncate -s 1G out/disk.raw
	sgdisk --clear --disk-guid="$DISK_GUID" \
		--new=1:2048:0 --typecode=1:EF00 --change-name=1:"$ESP_NAME" \
		--partition-guid=1:"$ESP_GUID" out/disk.raw >/dev/null
	first="$(sgdisk --info=1 out/disk.raw | sed -n 's/^First sector: \([0-9]*\).*/\1/p')"
	last="$(sgdisk --info=1 out/disk.raw | sed -n 's/^Last sector: \([0-9]*\).*/\1/p')"
	[[ "$first" == 2048 && -n "$last" ]] || die "unexpected ESP layout ($first..$last)"
	# mkfs.fat counts BLOCKS in KiB.
	mkfs.vfat -F 32 --invariant -i "$FAT_VOLID" -n "$FAT_LABEL" -h "$first" \
		--offset="$first" out/disk.raw $(((last - first + 1) / 2)) >/dev/null
	# mtools stamps new entries with SOURCE_DATE_EPOCH when it is set.
	touch -d "@$SOURCE_DATE_EPOCH" out/BOOTX64.EFI
	img="out/disk.raw@@$((first * 512))"
	MTOOLS_SKIP_CHECK=1 mmd -i "$img" ::/EFI ::/EFI/BOOT
	MTOOLS_SKIP_CHECK=1 mcopy -m -i "$img" out/BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
	touch -d "@$SOURCE_DATE_EPOCH" out/disk.raw
	rm -f out/vcontainer.tar.gz
	tar --format=oldgnu --owner=0 --group=0 --numeric-owner \
		-Sczf out/vcontainer.tar.gz -C out disk.raw

	python3 tools/predict-rtmr1.py out/BOOTX64.EFI out/disk.raw >out/rtmr1.txt
	cd out
	sha256sum init BOOTX64.EFI disk.raw vcontainer.tar.gz
	cat rtmr1.txt
	exit 0
fi

# ---------------------------------------------------------------------------
# On the host.
# ---------------------------------------------------------------------------
init=target/x86_64-unknown-linux-musl/release/vcontainer-init
clean=0
while [[ $# -gt 0 ]]; do
	case "$1" in
		--init) init="${2:?--init needs a path}"; shift 2 ;;
		--clean) clean=1; shift ;;
		-h | --help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
		*) die "unknown argument: $1" ;;
	esac
done

cd "$repo"
[[ -f "$init" ]] || die "missing init binary $init (build it, or pass --init PATH)"
file -b "$init" 2>/dev/null | grep -q 'statically linked\|static-pie' ||
	echo "build-image: warning: $init does not look statically linked" >&2
[[ -f ".cache/linux-$KVER.tar.xz" ]] ||
	die "missing .cache/linux-$KVER.tar.xz (https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz)"

if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
	SOURCE_DATE_EPOCH="$(git log -1 --format=%ct 2>/dev/null || true)"
	SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$FALLBACK_EPOCH}"
fi
export SOURCE_DATE_EPOCH

mkdir -p out
[[ "$clean" == 0 ]] || rm -rf ".cache/kbuild-$KVER"
# Copy /init into out/ only when it changed (the kernel relinks on a newer
# mtime). The copy keeps the list path fixed whatever --init points at.
cmp -s "$init" out/init || cp "$init" out/init
chmod 0755 out/init

docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -e SOURCE_DATE_EPOCH \
	-v "$repo:/work" -w /work "$BUILDER" bash scripts/build-image.sh --inside
