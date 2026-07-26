# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How this file works

**Every merge to main cuts a release** (see [`RELEASING.md`](RELEASING.md)), so
this file is written by contributors, not by the release job. Add a bullet under
`## [Unreleased]` in the same PR as your change, whenever that change is
something a user would notice — a new flag, a changed default, a fixed bug, a
breaking rename.

On release, `auto-tag.yml` moves whatever is under `## [Unreleased]` beneath a
new version heading and leaves `## [Unreleased]` empty for the next change. It
does not invent entries: a release with nothing under `## [Unreleased]` gets a
version heading with no bullets, which is the honest record of a merge that
changed nothing user-facing.

Internal refactors, test-only changes, and CI work do not need an entry.

Each GitHub Release additionally carries per-release notes generated at publish
time from the commit range. Those are a summary of *commits*; this file is a
record of *changes*, curated by the person who made them.

## [Unreleased]

## [0.5.29] — 2026-07-26

## [0.5.28] — 2026-07-25

## Before 0.5.27

This file was introduced at 0.5.27. Earlier releases are recorded only in their
generated GitHub Release notes, at
<https://github.com/macanderson/stella/releases>. No attempt has been made to
reconstruct them here — a hand-written history of releases nobody curated at the
time would be a guess presented as a record.
