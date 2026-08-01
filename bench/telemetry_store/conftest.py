"""Make `ingest` importable from `tests/` without installing a package.

`bench/telemetry_store/` is a pair of loose files, not a distribution, and
pytest's default import mode only puts the *test* directory on `sys.path`.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
