# shellcheck shell=sh
#
# Sourced helper: the one implementation of the `--help` printers in
# scripts/ (#3350). Source it relative to the calling script, before any
# `cd`, so `$0` still resolves:
#
#   # shellcheck source=scripts/lib/help-header.sh
#   . "$(dirname "$0")/lib/help-header.sh"
#   usage() { print_help_header "$0"; }

# Print FILE's leading `#` comment block — everything after the shebang,
# uncommented. Stops at the sentinel line
#
#   # --help text ends here.
#
# when the header carries one (so the operational WHY prose below it stays
# out of --help), or at the first non-comment line otherwise. Selecting by
# content instead of a hardcoded line range is the point: a range truncates
# silently the first time the header grows (#3350).
print_help_header() {
  awk '
    NR == 1 { next }
    /^# --help text ends here\.[[:space:]]*$/ { exit }
    !/^#/ { exit }
    { sub(/^# ?/, ""); print }
  ' "$1"
}
