#!/usr/bin/env bash
#
# Roll CHANGELOG.md's `[Unreleased]` section under a new version heading.
#
# Usage: NEW_VERSION=0.7.0 [RELEASE_DATE=2026-08-07] \
#          [CHANGELOG_ENTRIES_FILE=<path>] scripts/changelog-roll.sh [<file>]
#
# Split out of scripts/sync-versions.sh so it can be tested without standing up
# a Cargo workspace (scripts/test-changelog-roll.sh, `make changelog-roll-test`).
# sync-versions.sh calls it at both of its call sites — the tagged release
# commit and the bot/version-sync PR — so the tag and main roll identical text.
#
# ## The rule: minor and major lines only
#
# Every merge to main cuts a patch release, so this roll used to fire ~130
# times per minor line. scripts/changelog-ai.sh degrades open by contract (no
# API key, no non-bot commits, or a response that does not look like changelog
# markdown all print nothing and exit 0), but the roll ran regardless —
# depositing a bare `## [0.6.x] — <date>` heading with nothing under it. That is
# how CHANGELOG.md reached 180 sections of which 77 were empty: the file was
# structurally guaranteed to fill with noise no matter how good the drafter got.
#
# So a patch release does not touch this file at all. Its detail is not lost —
# release.yml publishes GitHub Release notes for every tag, which is the surface
# built for "what changed in this exact build". CHANGELOG.md is the curated
# durable record: one section per minor line, drafted once from the whole series
# range. RELEASING.md § "What records a change" documents the split.
#
# ## The second rule: never emit a heading with nothing under it
#
# A minor release whose draft failed would otherwise reproduce the original
# defect once per line. When the drafter degrades open AND `[Unreleased]` is
# empty, this writes a pointer to the releases page instead — truthful, because
# the per-tag detail genuinely does exist there. A bare heading is never an
# acceptable output of this script.
#
# Tolerant everywhere else: a missing file or a missing
# `[Unreleased]` heading is a warning, not a failure. A changelog bookkeeping
# slip must never be the reason a release fails to ship.
#
# perl, not `sed -i "0,/re/"`: that address form is GNU-only and SILENTLY
# no-ops on BSD sed (macOS).
set -euo pipefail

version="${NEW_VERSION:?set NEW_VERSION to the bare semver, e.g. 0.7.0}"
export NEW_VERSION
changelog="${1:-CHANGELOG.md}"

# The releases page carries every tag's notes; the fallback body points there.
releases_url="https://github.com/macanderson/stella/releases"

# Checking the patch component here — rather than trusting a caller to pass the
# bump kind — keeps the rule true at both call sites with no third place to
# drift.
case "${version}" in
  *.*.0) ;;
  *)
    echo "changelog-roll: ${version} is a patch release; CHANGELOG.md records minor and major lines only."
    exit 0
    ;;
esac

if [ ! -f "${changelog}" ] || ! grep -q '^## \[Unreleased\]' "${changelog}"; then
  echo "::warning::${changelog} missing or has no [Unreleased] heading; skipping the roll."
  exit 0
fi

# A version that already has a section keeps every line of it.
#
# The roll runs at two call sites per release: the tagged release commit and
# the bot/version-sync PR. A maintainer may also write the section by hand in
# the release PR. A minor release is a considered event, and the "CI writes
# this file" rule exists to keep one voice in the file. It does not exist to
# overwrite a section someone sat down and wrote.
#
# So the roll keeps every line of the section and writes no second heading.
# It adds only draft bullets whose PR refs the section does not cite. It leaves
# out a draft bullet that cites no PR, or any PR the section already cites, and
# it leaves out the draft's own prose. A kept bullet goes under the section's
# matching `###` heading when it has one, and under a new heading when it does
# not.
#
# A bullet that lands makes its refs cited. The second call site then finds
# nothing new and leaves the file as it is.
append_new_bullets() {
  perl - "$@" <<'PERL'
use strict;
use warnings;

my ($changelog, $entries_file, $version) = @ARGV;
sub slurp {
  my ($path) = @_;
  open my $fh, "<", $path or die "cannot open $path: $!";
  local $/;
  my $text = <$fh>;
  close $fh;
  return $text;
}
my $text = slurp($changelog);
my $draft = slurp($entries_file);

my $v = quotemeta $version;
if ($text !~ /^## \[$v\][^\n]*(?:\n|\z)/m) {
  print "0\n";
  exit 0;
}
my $body_start = $+[0];
my $body_end = length $text;
pos($text) = $body_start;
$body_end = $-[0] if $text =~ /^## \[/mg;
my $section = substr($text, $body_start, $body_end - $body_start);
my %cited = map { $_ => 1 } $section =~ /#(\d+)/g;

my @items;
my ($head, $bullet, $gap) = ("", undef, 0);
my $flush = sub {
  push @items, [$head, $bullet] if defined $bullet;
  $bullet = undef;
};
for my $line (split /\n/, $draft) {
  if ($line =~ /^\s*$/) {
    $gap = 1;
    next;
  }
  if ($line =~ /^#{1,6}\s/) {
    $flush->();
    $head = $line =~ /^###\s+(.*?)\s*$/ ? $1 : "";
  } elsif ($line =~ /^[-*+]\s/) {
    $flush->();
    $bullet = $line;
  } elsif (defined $bullet && $line =~ /^\s+\S/) {
    $bullet .= ($gap ? "\n\n" : "\n") . $line;
  } else {
    $flush->();
  }
  $gap = 0;
}
$flush->();

my (@order, %keep);
for my $item (@items) {
  my ($h, $entry) = @$item;
  my @refs = $entry =~ /#(\d+)/g;
  next if !@refs || grep { $cited{$_} } @refs;
  push @order, $h unless $keep{$h};
  push @{ $keep{$h} }, $entry;
}

my $added = 0;
for my $h (@order) {
  my $add = join "\n", @{ $keep{$h} };
  $added += @{ $keep{$h} };
  if ($h ne "" && $section =~ /^###[ \t]+\Q$h\E[ \t]*(?:\n|\z)/m) {
    my $sub_start = $+[0];
    my $sub_end = length $section;
    pos($section) = $sub_start;
    $sub_end = $-[0] if $section =~ /^#{2,3}[ \t]/mg;
    my $sub = substr($section, $sub_start, $sub_end - $sub_start);
    $sub =~ s/\s+\z//;
    substr($section, $sub_start, $sub_end - $sub_start) = "$sub\n$add\n\n";
  } else {
    $section =~ s/\s+\z//;
    $section .= ($section eq "" ? "\n" : "\n\n")
      . ($h ne "" ? "### $h\n\n" : "") . "$add\n";
  }
}

if ($added) {
  $section =~ s/\s+\z//;
  $section .= $body_end < length $text ? "\n\n" : "\n";
  substr($text, $body_start, $body_end - $body_start) = $section;
  open my $out, ">", $changelog or die "cannot write $changelog: $!";
  print {$out} $text;
  close $out or die "cannot write $changelog: $!";
}
print "$added\n";
PERL
}

if grep -q "^## \[${version}\]" "${changelog}"; then
  if [ -z "${CHANGELOG_ENTRIES_FILE:-}" ] || [ ! -s "${CHANGELOG_ENTRIES_FILE}" ]; then
    echo "changelog-roll: ${changelog} already has a [${version}] section; leaving it alone."
    exit 0
  fi
  if ! added="$(append_new_bullets "${changelog}" "${CHANGELOG_ENTRIES_FILE}" "${version}")"; then
    echo "::warning::could not add draft bullets to the existing [${version}] section; leaving it alone."
    exit 0
  fi
  if [ "${added}" = "0" ]; then
    echo "changelog-roll: ${changelog} already has a [${version}] section, and the draft cites no PR it lacks; leaving it alone."
  else
    echo "changelog-roll: appended ${added} draft bullet(s) to the existing [${version}] section, each citing only PRs it lacked."
  fi
  exit 0
fi

# Replace whatever sits under [Unreleased] with $1's contents.
inject_entries() {
  ENTRIES_FILE="$1" perl -0777 -pi -e '
    open my $fh, "<", $ENV{ENTRIES_FILE} or die "cannot open $ENV{ENTRIES_FILE}: $!";
    my $entries = do { local $/; <$fh> };
    close $fh;
    $entries =~ s/\s+\z//;
    s/^## \[Unreleased\]\n.*?(?=^## \[|\z)/"## [Unreleased]\n\n" . $entries . "\n\n"/mse;
  ' "${changelog}"
}

unreleased_body="$(awk '/^## \[Unreleased\]$/{f=1;next} /^## \[/{f=0} f' "${changelog}" | tr -d '[:space:]')"

if [ -n "${CHANGELOG_ENTRIES_FILE:-}" ] && [ -s "${CHANGELOG_ENTRIES_FILE}" ]; then
  # The CI draft is authoritative: it REPLACES whatever already sits under
  # [Unreleased], hand-written or not. Changelog authorship lives in the release
  # job, not in feature PRs, so entries stay in one voice instead of whatever a
  # contributor or coding agent happened to type.
  inject_entries "${CHANGELOG_ENTRIES_FILE}"
  echo "changelog-roll: CI-drafted entries written for ${version}."
elif [ -z "${unreleased_body}" ]; then
  fallback="$(mktemp)"
  printf '_The draft for this section could not be generated. Per-release notes for\nevery tag in this line are published on the [releases page](%s)._\n' \
    "${releases_url}" > "${fallback}"
  inject_entries "${fallback}"
  rm -f "${fallback}"
  echo "::warning::no entries drafted for ${version}; wrote a releases-page pointer rather than an empty section."
else
  echo "::warning::no entries drafted for ${version}; rolling the existing [Unreleased] body as-is."
fi

# Inserting *after* the heading (rather than renaming it) is what carries the
# section's content down into the new version's section.
RELEASE_DATE="${RELEASE_DATE:-$(date -u +%F)}" \
  perl -pi -e 's/^## \[Unreleased\]$/## [Unreleased]\n\n## [$ENV{NEW_VERSION}] — $ENV{RELEASE_DATE}/' "${changelog}"

grep -q "^## \[${version}\] " "${changelog}" \
  || echo "::warning::${changelog} roll did not take for ${version}"
