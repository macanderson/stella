# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Reading what a trial left behind — the file tree, and whether a video is real.

Two jobs that share a threat model.

**Browsing a trial.** A trial directory is the whole evidence trail: the result,
the agent's trajectory and event stream, the verifier's output, the recording.
Being able to open any of it in the browser is the difference between trusting a
scoreboard and being able to check it. The cost is that a local web server is
now handing out file contents, so every path that reaches the filesystem is
resolved and then *proved* to be inside the trial directory it claims to be in
(:func:`resolve_within`). Symlinks are resolved before the check, not after —
a trial directory is written by a container we did not author, and a symlink
pointing at ``/etc`` is exactly what a path-containment bug looks like in
practice.

**Deciding a recording is watchable.** An MP4 is not playable because a file
exists at that path. The recorder writes with ``-movflags +faststart``, which
places the index (``moov``) at the *end* of encoding — so for the entire
lifetime of a live trial the file is present, growing, and completely
unplayable, and if the run is killed it stays that way forever. Offering a
player in that window produces a broken box in the UI and a bug report about
video "not working" that is really a bug report about timing.

:func:`mp4_is_finalized` answers the real question by walking the top-level
boxes for a ``moov``. It is a header scan, not a decode: cheap enough to call
on every snapshot, and it cannot be fooled by a file that merely has bytes in
it. Fragmented MP4s (``+empty_moov``) carry a ``moov`` up front and so read as
finalized from the first frame, which is correct — those genuinely are playable
while being written.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "MAX_TEXT_BYTES",
    "FileNode",
    "mp4_is_finalized",
    "preview_kind",
    "resolve_within",
    "tree",
]

#: Read at most this much of a text file into the browser. Event streams reach
#: hundreds of megabytes; a viewer that tries to render one is a hung tab.
MAX_TEXT_BYTES = 2 * 1024 * 1024

#: Suffixes rendered as text. Everything else is offered as a download rather
#: than guessed at — a mis-detected binary is a screenful of replacement
#: characters, which looks like corruption and is not.
TEXT_SUFFIXES = frozenset(
    {
        ".txt", ".log", ".json", ".jsonl", ".md", ".toml", ".yaml", ".yml",
        ".py", ".sh", ".rs", ".js", ".ts", ".css", ".html", ".xml", ".csv",
        ".patch", ".diff", ".ini", ".cfg", ".env", ".c", ".h", ".cpp", ".go",
    }
)
VIDEO_SUFFIXES = frozenset({".mp4", ".webm", ".mov"})
IMAGE_SUFFIXES = frozenset({".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp"})

#: Never walk into these. They are large, uninteresting, and in the case of
#: `.git` can contain a full copy of the workspace.
SKIP_DIRS = frozenset({".git", "__pycache__", "node_modules"})


# ---------------------------------------------------------------------------
# Path containment
# ---------------------------------------------------------------------------


def resolve_within(root: Path, relative: str) -> Path | None:
    """Resolve ``relative`` under ``root``, or ``None`` if it escapes.

    The containment check happens **after** symlink resolution on both sides,
    because a trial directory is produced by a container this process did not
    author. Checking the textual path first and resolving later is the classic
    ordering bug: ``arena/link`` looks contained right up until it is followed.

    An absolute path, a ``..`` segment, or an NT-style drive letter is refused
    outright rather than normalised — nothing legitimate in a trial tree needs
    them, so accepting them only widens what has to be reasoned about.
    """
    candidate = (relative or "").strip().replace("\\", "/")
    if not candidate or candidate.startswith("/") or ":" in candidate:
        return None
    if any(part == ".." for part in candidate.split("/")):
        return None
    try:
        base = root.resolve(strict=True)
        target = (base / candidate).resolve(strict=True)
    except (OSError, RuntimeError):  # missing, or a symlink loop
        return None
    if target != base and base not in target.parents:
        return None
    return target


# ---------------------------------------------------------------------------
# The tree
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class FileNode:
    """One entry in a trial's artifact tree."""

    name: str
    #: Path relative to the trial directory — what a client sends back.
    path: str
    is_dir: bool
    size: int
    kind: str
    children: tuple[FileNode, ...] = ()

    def to_json(self) -> dict[str, object]:
        node: dict[str, object] = {
            "name": self.name,
            "path": self.path,
            "is_dir": self.is_dir,
            "size": self.size,
            "kind": self.kind,
        }
        if self.is_dir:
            node["children"] = [child.to_json() for child in self.children]
        return node


def preview_kind(path: Path) -> str:
    """How a client should render this file: text, video, image, or binary."""
    suffix = path.suffix.lower()
    if suffix in VIDEO_SUFFIXES:
        return "video"
    if suffix in IMAGE_SUFFIXES:
        return "image"
    if suffix in TEXT_SUFFIXES:
        return "text"
    # Extensionless files are common in these trees (`test.sh` aside, think
    # `Dockerfile`); sniff a small window rather than refusing them.
    try:
        with path.open("rb") as handle:
            head = handle.read(4096)
    except OSError:
        return "binary"
    if not head:
        return "text"
    if b"\x00" in head:
        return "binary"
    try:
        head.decode("utf-8")
    except UnicodeDecodeError:
        return "binary"
    return "text"


def tree(root: Path, *, max_depth: int = 8, max_entries: int = 2000) -> FileNode:
    """The artifact tree under ``root``, depth- and breadth-capped.

    The caps are not tidiness. A trial that ran ``git init`` or unpacked a
    dependency tree can leave tens of thousands of files, and a browser handed
    all of them renders nothing at all.
    """
    budget = [max_entries]

    def walk(directory: Path, relative: str, depth: int) -> FileNode:
        children: list[FileNode] = []
        if depth < max_depth and budget[0] > 0:
            try:
                entries = sorted(
                    directory.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower())
                )
            except OSError:
                entries = []
            for entry in entries:
                if budget[0] <= 0:
                    break
                budget[0] -= 1
                child_rel = f"{relative}/{entry.name}" if relative else entry.name
                # `is_dir()` follows symlinks; a link out of the tree must not
                # be descended into even though it is contained by name.
                if entry.is_symlink():
                    children.append(
                        FileNode(entry.name, child_rel, False, 0, "symlink")
                    )
                    continue
                if entry.is_dir():
                    if entry.name in SKIP_DIRS:
                        children.append(
                            FileNode(entry.name, child_rel, True, 0, "skipped")
                        )
                        continue
                    children.append(walk(entry, child_rel, depth + 1))
                else:
                    try:
                        size = entry.stat().st_size
                    except OSError:
                        size = 0
                    children.append(
                        FileNode(
                            entry.name, child_rel, False, size, preview_kind(entry)
                        )
                    )
        return FileNode(
            name=directory.name,
            path=relative,
            is_dir=True,
            size=0,
            kind="dir",
            children=tuple(children),
        )

    return walk(root, "", 0)


# ---------------------------------------------------------------------------
# Is this recording watchable
# ---------------------------------------------------------------------------


def mp4_is_finalized(path: Path) -> bool:
    """Whether an MP4 carries the index that makes it playable.

    Walks top-level boxes looking for ``moov``. Three things make this the
    right check rather than "does the file exist":

    * A live recording is `ftyp | free | mdat(size=0)` — bytes accumulating
      into a box the writer has not closed. There is no index and no player
      can open it.
    * A recording killed before finalisation stays in exactly that state
      permanently. Existence never becomes playability on its own.
    * A fragmented recording (``+empty_moov``) writes ``moov`` up front, so it
      reads finalized immediately — which is true, and is the point of writing
      fragmented in the first place.
    """
    try:
        size = path.stat().st_size
    except OSError:
        return False
    if size < 16:
        return False
    try:
        with path.open("rb") as handle:
            offset = 0
            while offset < size:
                handle.seek(offset)
                header = handle.read(8)
                if len(header) < 8:
                    return False
                box_size, kind = struct.unpack(">I4s", header)
                if kind == b"moov":
                    return True
                if box_size == 0:
                    # Runs to end of file: an unclosed box, and nothing can
                    # follow it. If we have not seen `moov` by now, there is none.
                    return False
                if box_size == 1:
                    extended = handle.read(8)
                    if len(extended) < 8:
                        return False
                    box_size = struct.unpack(">Q", extended)[0]
                if box_size < 8:
                    return False
                offset += box_size
    except OSError:
        return False
    return False
