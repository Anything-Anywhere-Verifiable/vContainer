#!/usr/bin/env bash
# Assert that a final kernel .config matches image/kernel.config.
#
#   image/check-config.sh FINAL_DOT_CONFIG
#
# Every CONFIG_X=value line of kernel.config must appear verbatim in the final
# .config, and no "# CONFIG_X is not set" symbol may be =y or =m there.
# Kconfig only warns when a request is dropped (unmet dependency) or a symbol
# is forced on by `select`; this script fails instead.
set -euo pipefail

final="${1:?usage: check-config.sh FINAL_DOT_CONFIG}"
[[ -r "$final" ]] || { echo "check-config: cannot read $final" >&2; exit 2; }

fail=0 on=0 off=0
while IFS= read -r line; do
	case "$line" in
		CONFIG_*=*) sym="${line%%=*}" ;;
		"# CONFIG_"*" is not set") sym="${line#\# }"; sym="${sym%% *}" ;;
		*) continue ;;
	esac
	got="$(grep -E "^(# )?${sym}[= ]" "$final" || true)"
	if [[ "$line" == CONFIG_* ]]; then
		on=$((on + 1))
		[[ "$got" == "$line" ]] && continue
	else
		off=$((off + 1))
		[[ "$got" != CONFIG_* ]] && continue
	fi
	printf 'MISMATCH %s\n  want: %s\n  got:  %s\n' "$sym" "$line" "${got:-<absent>}"
	fail=1
done <"$(dirname "$0")/kernel.config"

if [[ "$fail" != 0 ]]; then
	echo "check-config: $final does not match image/kernel.config" >&2
	exit 1
fi
echo "check-config: OK ($on requested values, $off forbidden symbols)"
