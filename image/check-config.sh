#!/usr/bin/env bash
# Assert that a final kernel .config matches what we asked for.
#
#   image/check-config.sh FINAL_DOT_CONFIG [MINICONFIG] [MUST_BE_OFF]
#
# Every CONFIG_X=value line of the miniconfig (default image/kernel.config)
# must appear verbatim in the final .config, and no symbol listed in the
# must-be-off file (default image/kernel.must-be-off) may be =y or =m.
# Kconfig and merge_config.sh only warn when a request is dropped (unmet
# dependency) or a symbol is forced on by `select`; this script fails instead.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
final="${1:?usage: check-config.sh FINAL_DOT_CONFIG [MINICONFIG] [MUST_BE_OFF]}"
mini="${2:-$here/kernel.config}"
off="${3:-$here/kernel.must-be-off}"

fail=0
# Requested values: compare the whole "CONFIG_X=value" line.
while IFS= read -r line; do
	sym="${line%%=*}"
	got="$(grep -E "^(# )?${sym}[= ]" "$final" || true)"
	if [[ "$got" != "$line" ]]; then
		printf 'MISMATCH %s\n  want: %s\n  got:  %s\n' "$sym" "$line" "${got:-<absent>}"
		fail=1
	fi
done < <(grep -E '^CONFIG_[A-Za-z0-9_]+=' "$mini")

# Must-be-off symbols: absent or "is not set" are both fine.
while IFS= read -r sym; do
	if got="$(grep -E "^CONFIG_${sym}=" "$final")"; then
		printf 'FORBIDDEN %s\n  want: # CONFIG_%s is not set\n  got:  %s\n' "$sym" "$sym" "$got"
		fail=1
	fi
done < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$off" | grep -v '^$')

if [[ "$fail" != 0 ]]; then
	echo "check-config: $final does not match $mini / $off" >&2
	exit 1
fi
echo "check-config: OK ($(grep -cE '^CONFIG_[A-Za-z0-9_]+=' "$mini") requested values, $(sed -e 's/#.*//' "$off" | grep -c '[A-Z]') forbidden symbols)"
