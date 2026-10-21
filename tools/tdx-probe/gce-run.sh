#!/usr/bin/env bash
# Boot the probe image on GCE C3 TDX VMs and collect a pass/fail result.
#
#   PROJECT=... BUCKET=... tools/tdx-probe/gce-run.sh [SHAPE...]
#
# Uploads out/vcontainer.tar.gz (kept), boots one VM per SHAPE, saves serial
# logs to out/probe-serial-<SHAPE>.log, and deletes only this run's VMs/image.
# PROBE DONE means collection ended; only PROBE RESULT PASS is a passing run.
# Build first: scripts/build-image.sh --dev --init "$(tools/tdx-probe/build.sh)"
set -euo pipefail

PROJECT="${PROJECT:?set PROJECT to your Google Cloud project ID}"
ZONE="${ZONE:-us-central1-a}"
REGION="${ZONE%-*}"
BUCKET="${BUCKET:?set BUCKET to a Cloud Storage bucket in that project}"
NETWORK=vcontainer
POLL_TIMEOUT="${PROBE_POLL_TIMEOUT:-600}"
POLL_INTERVAL="${PROBE_POLL_INTERVAL:-10}"
[[ "$POLL_TIMEOUT" =~ ^[0-9]+$ && "$POLL_INTERVAL" =~ ^[0-9]+$ ]] || {
    echo 'probe: PROBE_POLL_TIMEOUT and PROBE_POLL_INTERVAL must be nonnegative integers' >&2
    exit 2
}

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tarball="$repo/out/vcontainer.tar.gz"
[[ -f "$tarball" ]] || { echo "missing $tarball" >&2; exit 1; }
shapes=("$@")
[[ ${#shapes[@]} -gt 0 ]] || shapes=(c3-standard-4)

# Names alone are not ownership. Record successful create IDs, then recheck
# both the immutable ID and this unpredictable run label before each delete.
run_id="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
[[ "$run_id" =~ ^[0-9a-f]{32}$ ]] || { echo 'probe: cannot generate run ID' >&2; exit 1; }
image="vcontainer-probe-$run_id"
object="gs://$BUCKET/$image.tar.gz"
g=(gcloud --project "$PROJECT" --quiet)
vms=()
vm_ids=()
image_id=

created_id() {
    local id="$1"
    [[ "$id" =~ ^[0-9]+$ ]] || {
        echo "probe: create returned no resource ID; inspect resources labeled vcontainer_run=$run_id" >&2
        return 1
    }
    printf '%s' "$id"
}

owned_delete() {
    local kind="$1" name="$2" expected_id="$3" identity actual_id actual_run extra
    local zone_args=()
    [[ "$kind" != instances ]] || zone_args=(--zone "$ZONE")
    # Old Bash treats an empty array as unset with nounset; expand conditionally.
    if ! identity="$("${g[@]}" compute "$kind" describe "$name" ${zone_args[@]+"${zone_args[@]}"} --format='value(id,labels.vcontainer_run)')"; then
        echo "WARNING: cannot establish ownership of $kind $name; not deleting" >&2
        return 1
    fi
    IFS=$'\t' read -r actual_id actual_run extra <<< "$identity"
    if [[ "$actual_id" != "$expected_id" || "$actual_run" != "$run_id" || -n "$extra" ]]; then
        echo "WARNING: ownership changed for $kind $name; not deleting" >&2
        return 1
    fi
    if ! "${g[@]}" compute "$kind" delete "$name" ${zone_args[@]+"${zone_args[@]}"}; then
        echo "WARNING: could not delete $kind $name" >&2
        return 1
    fi
    echo "deleted $kind $name"
}

cleanup() {
    local status=$? cleanup_failed=0 i
    trap - EXIT
    set +e
    for ((i=0; i<${#vms[@]}; i++)); do
        owned_delete instances "${vms[$i]}" "${vm_ids[$i]}" || cleanup_failed=1
    done
    if [[ -n "$image_id" ]]; then
        owned_delete images "$image" "$image_id" || cleanup_failed=1
    fi
    if [[ "$cleanup_failed" != 0 && "$status" == 0 ]]; then status=1; fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

echo "uploading $object"
"${g[@]}" storage cp "$tarball" "$object"
image_id="$("${g[@]}" compute images create "$image" --source-uri "$object" \
    --guest-os-features UEFI_COMPATIBLE,GVNIC,TDX_CAPABLE \
    --storage-location "$REGION" --labels "vcontainer=probe,vcontainer_run=$run_id" --format='value(id)')"
image_id="$(created_id "$image_id")"

if ! "${g[@]}" compute networks describe "$NETWORK" >/dev/null 2>&1; then
    "${g[@]}" compute networks create "$NETWORK" --subnet-mode auto
fi

for ((i=0; i<${#shapes[@]}; i++)); do
    shape="${shapes[$i]}"
    vm="$image-$i"
    id="$("${g[@]}" compute instances create "$vm" --zone "$ZONE" --machine-type "$shape" \
        --confidential-compute-type TDX --maintenance-policy TERMINATE --no-restart-on-failure \
        --no-shielded-secure-boot --shielded-vtpm \
        --image "$image" --boot-disk-size 10GB --boot-disk-type pd-balanced \
        --network-interface "network=$NETWORK,nic-type=GVNIC,stack-type=IPV4_ONLY" \
        --no-service-account --no-scopes --metadata vcontainer-owner=probe \
        --labels "vcontainer=probe,vcontainer_run=$run_id" --format='value(id)')"
    id="$(created_id "$id")"
    vms+=("$vm")
    vm_ids+=("$id")
done

# Retain per-VM completion flags, avoiding empty-array behavior in older Bash.
deadline=$((SECONDS + 10#$POLL_TIMEOUT))
remaining=${#vms[@]}
finished=()
for ((i=0; i<${#vms[@]}; i++)); do finished+=(0); done
failed=0
while ((remaining > 0)); do
    for ((i=0; i<${#vms[@]}; i++)); do
        [[ "${finished[$i]}" == 0 ]] || continue
        log="$repo/out/probe-serial-${shapes[$i]}.log"
        "${g[@]}" compute instances get-serial-port-output "${vms[$i]}" --zone "$ZONE" >"$log" 2>/dev/null || true
        if grep -Eq '(^PANIC:|Kernel panic)' "$log"; then
            echo "${vms[$i]}: probe failed -> $log" >&2
            failed=1
        elif grep -q '^PROBE DONE$' "$log"; then
            if grep -Eq '(^ERR |^PROBE RESULT FAIL)' "$log"; then
                echo "${vms[$i]}: probe checks failed -> $log" >&2
                failed=1
            elif grep -q '^PROBE RESULT PASS$' "$log"; then
                echo "${vms[$i]}: PROBE RESULT PASS -> $log"
            else
                echo "${vms[$i]}: collection ended without PROBE RESULT PASS -> $log" >&2
                failed=1
            fi
        elif ((SECONDS >= deadline)); then
            echo "${vms[$i]}: timeout waiting for PROBE DONE -> $log" >&2
            failed=1
        else
            continue
        fi
        finished[$i]=1
        remaining=$((remaining - 1))
    done
    ((remaining == 0)) || sleep "$POLL_INTERVAL"
done
exit "$failed"
