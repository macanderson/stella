# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""ArenaBench — coding-agent benchmarks as a live, side-by-side contest.

Pick a benchmark, pick the tasks, seat N contestants (an agent plus a full
engine configuration plus its own environment), and watch them race the same
task list in real time: a tournament scoreboard across seven dimensions, live
transcripts per task, and a real MP4 screen recording of each trial.

The design constraint throughout is that watching must not perturb. Everything
displayed is read from artifacts the run already writes, so the arena works
identically on a live match and an archive, and the act of observing a
contestant can never change its number.
"""

from __future__ import annotations

__version__ = "0.1.0"

from .model import (  # noqa: E402
    ARENA_COLORS,
    DIMENSIONS,
    EFFORTS,
    ROLES,
    Contestant,
    Dimension,
    Engine,
    MatchSpec,
    RoleConfig,
)
from .registry import DEFAULT_REGISTRY, Dataset, Registry, Task  # noqa: E402

__all__ = [
    "ARENA_COLORS",
    "DEFAULT_REGISTRY",
    "DIMENSIONS",
    "EFFORTS",
    "ROLES",
    "Contestant",
    "Dataset",
    "Dimension",
    "Engine",
    "MatchSpec",
    "Registry",
    "RoleConfig",
    "Task",
    "__version__",
]
