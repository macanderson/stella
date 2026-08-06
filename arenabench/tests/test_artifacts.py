# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Browsing a trial, and deciding a recording is watchable.

Two things are load-bearing here. The containment rules decide whether a local
web server hands out files it should not, and the MP4 check decides whether the
UI offers a player for something no player can open.
"""

from __future__ import annotations

import struct

import pytest

from arenabench.artifacts import (
    FileNode,
    mp4_is_finalized,
    preview_kind,
    resolve_within,
    tree,
)


def _box(kind: bytes, payload: bytes = b"") -> bytes:
    return struct.pack(">I4s", 8 + len(payload), kind) + payload


def _ftyp() -> bytes:
    return _box(b"ftyp", b"isom" + b"\x00" * 20)


# -- containment ------------------------------------------------------------


@pytest.mark.parametrize(
    "attempt",
    [
        "../../../etc/passwd",
        "..",
        "arena/../../escape",
        "/etc/passwd",
        "/",
        "C:/Windows/win.ini",
        "",
        "   ",
    ],
)
def test_paths_that_leave_the_trial_are_refused(tmp_path, attempt):
    (tmp_path / "arena").mkdir()
    (tmp_path / "arena" / "recording.mp4").write_bytes(b"x")
    assert resolve_within(tmp_path, attempt) is None


def test_a_real_file_inside_the_trial_resolves(tmp_path):
    (tmp_path / "arena").mkdir()
    target = tmp_path / "arena" / "recording.mp4"
    target.write_bytes(b"x")
    assert resolve_within(tmp_path, "arena/recording.mp4") == target.resolve()


def test_a_symlink_pointing_out_of_the_trial_is_refused(tmp_path):
    """The containment check must happen after resolution, not before.

    A trial directory is written by a container we did not author. `arena/out`
    is textually contained right up until it is followed.
    """
    outside = tmp_path.parent / "outside-secret"
    outside.write_text("secret", encoding="utf-8")
    trial = tmp_path / "trial"
    (trial / "arena").mkdir(parents=True)
    link = trial / "arena" / "out"
    link.symlink_to(outside)
    assert resolve_within(trial, "arena/out") is None


def test_a_symlink_staying_inside_the_trial_is_allowed(tmp_path):
    trial = tmp_path / "trial"
    (trial / "arena").mkdir(parents=True)
    real = trial / "arena" / "real.log"
    real.write_text("hi", encoding="utf-8")
    (trial / "alias.log").symlink_to(real)
    assert resolve_within(trial, "alias.log") == real.resolve()


def test_a_missing_file_is_refused_not_invented(tmp_path):
    assert resolve_within(tmp_path, "arena/nope.txt") is None


# -- the tree ---------------------------------------------------------------


def test_tree_lists_a_trial(tmp_path):
    (tmp_path / "agent").mkdir()
    (tmp_path / "agent" / "events.jsonl").write_text("{}\n", encoding="utf-8")
    (tmp_path / "result.json").write_text("{}", encoding="utf-8")
    node = tree(tmp_path)
    names = {child.name for child in node.children}
    assert {"agent", "result.json"} <= names
    agent = next(c for c in node.children if c.name == "agent")
    assert agent.is_dir
    assert agent.children[0].path == "agent/events.jsonl"


def test_tree_does_not_descend_into_git(tmp_path):
    """A trial that ran `git init` can hold a whole second copy of the workspace."""
    (tmp_path / ".git" / "objects").mkdir(parents=True)
    (tmp_path / ".git" / "objects" / "blob").write_text("x", encoding="utf-8")
    node = tree(tmp_path)
    git = next(c for c in node.children if c.name == ".git")
    assert git.kind == "skipped"
    assert git.children == ()


def test_tree_does_not_follow_symlinked_directories(tmp_path):
    target = tmp_path / "target"
    (target / "deep").mkdir(parents=True)
    trial = tmp_path / "trial"
    trial.mkdir()
    (trial / "link").symlink_to(target)
    node = tree(trial)
    link = next(c for c in node.children if c.name == "link")
    assert link.kind == "symlink"
    assert link.children == ()


def test_tree_is_breadth_capped(tmp_path):
    for i in range(50):
        (tmp_path / f"f{i}.txt").write_text("x", encoding="utf-8")
    node = tree(tmp_path, max_entries=10)
    assert len(node.children) <= 10


def test_file_node_json_omits_children_for_files():
    node = FileNode("a.txt", "a.txt", False, 3, "text")
    assert "children" not in node.to_json()


# -- preview kinds ----------------------------------------------------------


def test_preview_kinds(tmp_path):
    (tmp_path / "a.jsonl").write_text("{}", encoding="utf-8")
    (tmp_path / "b.mp4").write_bytes(b"\x00\x00")
    (tmp_path / "c.png").write_bytes(b"\x89PNG")
    assert preview_kind(tmp_path / "a.jsonl") == "text"
    assert preview_kind(tmp_path / "b.mp4") == "video"
    assert preview_kind(tmp_path / "c.png") == "image"


def test_an_extensionless_binary_is_not_offered_as_text(tmp_path):
    blob = tmp_path / "core"
    blob.write_bytes(b"\x7fELF\x00\x01\x02\x00\x00")
    assert preview_kind(blob) == "binary"


def test_an_extensionless_text_file_is_readable(tmp_path):
    script = tmp_path / "Dockerfile"
    script.write_text("FROM alpine\n", encoding="utf-8")
    assert preview_kind(script) == "text"


# -- is the recording watchable --------------------------------------------


def test_a_live_recording_is_not_finalized(tmp_path):
    """`ftyp | free | mdat(size=0)` is exactly what a running ffmpeg leaves.

    This is the state the UI used to offer a player for, and no player can
    open it.
    """
    video = tmp_path / "recording.mp4"
    video.write_bytes(
        _ftyp() + _box(b"free") + struct.pack(">I4s", 0, b"mdat") + b"\xde\xad" * 4096
    )
    assert mp4_is_finalized(video) is False


def test_a_finalized_recording_is_playable(tmp_path):
    video = tmp_path / "recording.mp4"
    video.write_bytes(_ftyp() + _box(b"moov", b"\x00" * 64) + _box(b"mdat", b"\x01" * 128))
    assert mp4_is_finalized(video) is True


def test_a_fragmented_recording_reads_as_finalized_immediately(tmp_path):
    """`+empty_moov` puts the index up front; those files really are playable."""
    video = tmp_path / "recording.mp4"
    video.write_bytes(
        _ftyp() + _box(b"moov", b"\x00" * 32) + _box(b"moof", b"\x00" * 16)
    )
    assert mp4_is_finalized(video) is True


def test_a_missing_or_empty_file_is_not_finalized(tmp_path):
    assert mp4_is_finalized(tmp_path / "nope.mp4") is False
    empty = tmp_path / "empty.mp4"
    empty.write_bytes(b"")
    assert mp4_is_finalized(empty) is False


def test_a_nonsense_box_size_does_not_hang(tmp_path):
    """A truncated or hostile header must terminate the walk, not loop."""
    video = tmp_path / "bad.mp4"
    video.write_bytes(struct.pack(">I4s", 2, b"ftyp") + b"\x00" * 64)
    assert mp4_is_finalized(video) is False
