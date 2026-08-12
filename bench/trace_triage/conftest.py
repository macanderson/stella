"""Make the triage modules importable from `tests/` without installing a package.

`bench/trace_triage/` is a set of loose standard-library modules, deliberately —
the piece that decides what a bench run says about the product should not need
the benchmark harness installed to be testable. Pytest's default import mode
only puts the *test* directory on `sys.path`, so this puts the package directory
there too, the same way `bench/telemetry_store/conftest.py` does.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
