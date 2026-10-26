#!/usr/bin/env bash
# Build the vcontainer boot image: one EFI-stub PE (kernel + built-in
# initramfs + built-in cmdline) on a GPT disk with a single ESP.
#
#   scripts/build-image.sh [--init PATH] [--clean] [--dev]
#
#   --init PATH  static binary to install as /init: a dev build, needs --dev
#                (default: built from crates/init)
#   --clean      drop the cached kernel source and objects first (a release
#                always does)
#   --dev        allow uncommitted changes and --init (the result is not a release)
#
# Everything runs inside the vcontainer/builder:dev container, built from
# image/Containerfile (repo mounted at /work, host uid). The kernel source is
# .cache/linux-6.18.57.tar.xz (sha256 pinned), extracted to
# .cache/linux-6.18.57; kernel objects are cached in .cache/kbuild-6.18.57.
# Outputs in out/:
#   init                 the /init that image/initramfs.list embeds
#   BOOTX64.EFI          the PE (arch/x86/boot/bzImage)
#   kernel.config        the final kernel .config
#   disk.raw             1 GiB GPT disk, ESP only, \EFI\BOOT\BOOTX64.EFI
#   vcontainer.tar.gz    disk.raw packed for `gcloud compute images create`
#   rtmr1.txt            the RTMR1 predicted by tools/predict-rtmr1.py
#   build-info.json      commit, dirty flag, builder image id
set -euo pipefail

KVER=6.18.57
KSRC_SHA256=5bd9acdc82ec5efc3790ae840e5aaa40dfde7efff261cf6dde902dc570acf98c
BUILDER=vcontainer/builder:dev
# Fixed identifiers and ESP range so the GPT (measured into RTMR1) and the FAT
# are byte-for-byte reproducible. ESP_LAST is the last usable LBA of the
# 1 GiB disk (2097152 - 34).
DISK_GUID=7C0E0000-5643-4F4E-5441-494E45520001
ESP_GUID=7C0E0000-5643-4F4E-5441-494E45520002
ESP_NAME=vcontainer-esp
ESP_FIRST=2048 ESP_LAST=2097118
FAT_VOLID=7C0E5643
FAT_LABEL=VCONTAINER

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

	# Kernel source = the sha256-pinned tarball, verified whenever the tree is created.
	if [[ ! -d "$ksrc" ]]; then
		if [[ ! -f "$tarball" ]]; then
			curl -fL --retry 3 -o "$tarball.part" "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz"
			mv "$tarball.part" "$tarball"
		fi
		echo "$KSRC_SHA256  $tarball" | sha256sum -c --quiet - || { rm -f "$tarball"; die "$tarball: sha256 mismatch"; }
		rm -rf "$kobj" "$ksrc.part"; mkdir "$ksrc.part"
		tar -xf "$tarball" -C "$ksrc.part" --strip-components=1
		mv "$ksrc.part" "$ksrc"
	fi
	# Constant build time and identity: they end up in the PE, so in RTMR1.
	export SOURCE_DATE_EPOCH=1767225600 KBUILD_BUILD_TIMESTAMP='2026-01-01 00:00:00 UTC' KBUILD_BUILD_USER=vcontainer KBUILD_BUILD_HOST=vcontainer KBUILD_BUILD_VERSION=1
	# Set (even empty) LOCALVERSION stops setlocalversion from probing git
	# and appending "+".
	export LOCALVERSION=
	kmake=(make -C "$ksrc" O="$kobj" -j"$(nproc)")

	"${kmake[@]}" -s allnoconfig KCONFIG_ALLCONFIG=/work/image/kernel.config KCONFIG_WARN_UNKNOWN_SYMBOLS=1 KCONFIG_WERROR=1
	image/check-config.sh "$kobj/.config"
	"${kmake[@]}" bzImage
	image/check-config.sh "$kobj/.config"
	release="$(cat "$kobj/include/config/kernel.release")"
	[[ "$release" == "$KVER-vcontainer" ]] || die "unexpected kernel.release $release"
	cp "$kobj/arch/x86/boot/bzImage" out/BOOTX64.EFI
	cp "$kobj/.config" out/kernel.config

	# Disk: exactly 1 GiB, GPT with one ESP, FAT32, one file.
	rm -f out/disk.raw
	truncate -s 1G out/disk.raw
	sgdisk --clear --disk-guid="$DISK_GUID" \
		--new=1:$ESP_FIRST:$ESP_LAST --typecode=1:EF00 --change-name=1:"$ESP_NAME" \
		--partition-guid=1:"$ESP_GUID" out/disk.raw >/dev/null
	# mkfs.fat counts BLOCKS in KiB.
	mkfs.vfat -F 32 --invariant -i "$FAT_VOLID" -n "$FAT_LABEL" -h $ESP_FIRST \
		--offset=$ESP_FIRST out/disk.raw $(((ESP_LAST - ESP_FIRST + 1) / 2)) >/dev/null
	# mtools stamps new entries with SOURCE_DATE_EPOCH.
	img="out/disk.raw@@$((ESP_FIRST * 512))"
	MTOOLS_SKIP_CHECK=1 mmd -i "$img" ::/EFI ::/EFI/BOOT
	MTOOLS_SKIP_CHECK=1 mcopy -i "$img" out/BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
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
init=
clean=0
dev=0
while [[ $# -gt 0 ]]; do
	case "$1" in
		--init) init="${2:?--init needs a path}"; shift 2 ;;
		--clean) clean=1; shift ;;
		--dev) dev=1; shift ;;
		-h | --help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
		*) die "unknown argument: $1" ;;
	esac
done

cd "$repo"
rm -f out/build-info.json
# The builder mounts the whole checkout, so the whole checkout is the input.
# A release is a clean checkout of HEAD built with its own /init.
commit="$(git rev-parse HEAD)"
changes="$(git status --porcelain)"
dirty=false; [[ -z "$changes" && -z "$init" ]] || dirty=true
[[ "$dirty" == false || "$dev" == 1 ]] || die "not a release (uncommitted changes or --init); commit, or pass --dev"
mkdir -p out .cache
# A release starts from the verified tarball, never from cached source or objects.
[[ "$clean" == 0 && "$dirty" == true ]] || rm -rf ".cache/linux-$KVER" ".cache/kbuild-$KVER"
docker build -q -t "$BUILDER" -f image/Containerfile image >/dev/null
in_builder() { docker run --rm --pull=never -u "$(id -u):$(id -g)" -e HOME=/tmp -e CARGO_HOME=/work/.cache/cargo -v "$repo:/work" -w /work "$BUILDER" "$@"; }
if [[ -z "$init" ]]; then
	in_builder env RUSTFLAGS="--remap-path-prefix=/work=. --remap-path-prefix=/work/.cache/cargo=cargo" cargo build --locked --release -p vcontainer-init --target x86_64-unknown-linux-musl
	init=target/x86_64-unknown-linux-musl/release/vcontainer-init
fi
# Copy only on change: the kernel relinks the initramfs on a newer mtime.
cmp -s "$init" out/init || cp "$init" out/init
in_builder bash scripts/build-image.sh --inside
printf '{"commit": "%s", "dirty": %s, "builder": "%s"}\n' "$commit" "$dirty" "$(docker image inspect -f '{{.Id}}' "$BUILDER")" >out/build-info.json
cat out/build-info.json
