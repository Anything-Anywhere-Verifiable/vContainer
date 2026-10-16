#!/usr/bin/env bash
# M0: boot the probe image on real GCE C3 TDX VMs and collect the serial logs.
#
#   tools/tdx-probe/gce-run.sh [SHAPE...]        (default: c3-standard-4)
#
# Uploads out/vcontainer.tar.gz to gs://$BUCKET/vcontainer-probe-<ts>.tar.gz
# (kept), creates image vcontainer-probe-<ts>, boots one VM per SHAPE in
# $ZONE, polls each serial port until "PROBE DONE", saves it to
# out/probe-serial-<SHAPE>.log, then deletes the VMs and the image (also on
# error or Ctrl-C). Build the image first:
#   scripts/build-image.sh --init "$(tools/tdx-probe/build.sh)"
set -euo pipefail

PROJECT="${PROJECT:-agentlockbox}"
ZONE="${ZONE:-us-central1-a}"
REGION="${ZONE%-*}"
BUCKET="${BUCKET:-agentlockbox-cvm-images}"
NETWORK=vcontainer
POLL_TIMEOUT=600

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tarball="$repo/out/vcontainer.tar.gz"
[[ -f "$tarball" ]] || { echo "missing $tarball" >&2; exit 1; }
shapes=("$@")
[[ ${#shapes[@]} -gt 0 ]] || shapes=(c3-standard-4)

ts="$(date -u +%Y%m%d-%H%M%S)"
image="vcontainer-probe-$ts"
object="gs://$BUCKET/$image.tar.gz"
g=(gcloud --project "$PROJECT" --quiet)
vms=()

cleanup() {
	set +e
	for vm in "${vms[@]}"; do
		"${g[@]}" compute instances delete "$vm" --zone "$ZONE" >/dev/null 2>&1 &&
			echo "deleted instance $vm" || echo "WARNING: could not delete instance $vm" >&2
	done
	if "${g[@]}" compute images describe "$image" >/dev/null 2>&1; then
		"${g[@]}" compute images delete "$image" >/dev/null 2>&1 &&
			echo "deleted image $image" || echo "WARNING: could not delete image $image" >&2
	fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

echo "uploading $object"
"${g[@]}" storage cp "$tarball" "$object"
"${g[@]}" compute images create "$image" --source-uri "$object" \
	--guest-os-features UEFI_COMPATIBLE,GVNIC,TDX_CAPABLE \
	--storage-location "$REGION" --labels vcontainer=probe

if ! "${g[@]}" compute networks describe "$NETWORK" >/dev/null 2>&1; then
	"${g[@]}" compute networks create "$NETWORK" --subnet-mode auto
fi

for shape in "${shapes[@]}"; do
	vm="$image-${shape//[^a-z0-9]/}"
	vm="${vm:0:63}"
	vms+=("$vm")
	"${g[@]}" compute instances create "$vm" --zone "$ZONE" --machine-type "$shape" \
		--confidential-compute-type TDX --maintenance-policy TERMINATE --no-restart-on-failure \
		--no-shielded-secure-boot --shielded-vtpm \
		--image "$image" --boot-disk-size 10GB --boot-disk-type pd-balanced \
		--network-interface "network=$NETWORK,nic-type=GVNIC,stack-type=IPV4_ONLY" \
		--no-service-account --no-scopes \
		--metadata vcontainer-owner=probe --labels vcontainer=probe
done

# Poll every VM until it printed PROBE DONE, panicked (panic=-1 reboots, so
# a panic means a boot loop), or the deadline passed.
deadline=$((SECONDS + POLL_TIMEOUT))
pending=("${!vms[@]}")
while [[ ${#pending[@]} -gt 0 ]]; do
	left=()
	for i in "${pending[@]}"; do
		log="$repo/out/probe-serial-${shapes[$i]}.log"
		"${g[@]}" compute instances get-serial-port-output "${vms[$i]}" --zone "$ZONE" >"$log" 2>/dev/null || true
		if grep -q 'PROBE DONE' "$log"; then
			echo "${vms[$i]}: PROBE DONE -> $log"
		elif grep -q 'Kernel panic' "$log"; then
			echo "${vms[$i]}: kernel panic (boot loop) -> $log" >&2
		elif ((SECONDS >= deadline)); then
			echo "${vms[$i]}: timeout waiting for PROBE DONE -> $log" >&2
		else
			left+=("$i")
		fi
	done
	pending=("${left[@]}")
	[[ ${#pending[@]} -eq 0 ]] || sleep 10
done
