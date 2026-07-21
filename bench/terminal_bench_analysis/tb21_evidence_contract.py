"""Canonical task-partition contract for the preregistered TB 2.1 study."""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Sequence

from tb21_study_seed import TASK_IDENTITIES, TaskIdentity

STUDY_ID = "stella-tb21-hybrid-study-v1"
TASK_PARTITION_SCHEMA = "stella-tb21-task-partition-v1"
DEVELOPMENT_TASK_NAMES = (
    "fix-git",
    "filter-js-from-html",
    "kv-store-grpc",
    "large-scale-text-editing",
    "regex-log",
    "schemelike-metacircular-eval",
    "sqlite-with-gcov",
    "bn-fit-modify",
    "make-mips-interpreter",
    "train-fasttext",
)

_SPLIT_NAMES = ("development", "screen", "untouched")
_PARTITION_FIELDS = {
    "schema_version",
    "study_id",
    *_SPLIT_NAMES,
    "split_sha256",
}
_RECORD_FIELDS = {
    "task_name",
    "canonical_task_reference",
    "task_checksum",
}
_SHA256_RE = re.compile(r"[0-9a-f]{64}")
_TASK_REFERENCE_RE = re.compile(r"sha256:[0-9a-f]{64}")


def canonical_file_bytes(value: object) -> bytes:
    """Encode a JSON file using the study's byte-canonical representation."""
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        )
        + "\n"
    ).encode()


def canonical_body_bytes(value: object) -> bytes:
    """Encode a JSON value canonically without the file-ending newline."""
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode()


def _reject_duplicate_pairs(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key {key!r}")
        value[key] = item
    return value


def _reject_nonfinite(value: str) -> object:
    raise ValueError(f"non-finite JSON number {value!r}")


def parse_canonical_object(raw: bytes, *, label: str) -> dict[str, object]:
    """Parse one canonical JSON object, rejecting alternate byte encodings."""
    try:
        value = json.loads(
            raw,
            object_pairs_hook=_reject_duplicate_pairs,
            parse_constant=_reject_nonfinite,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise ValueError(f"{label} is not strict JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must contain a JSON object")
    try:
        expected = canonical_file_bytes(value)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{label} cannot be represented canonically: {exc}") from exc
    if raw != expected:
        raise ValueError(f"{label} bytes are not canonical JSON")
    return value


def build_task_partition(
    identities: Sequence[TaskIdentity],
) -> dict[str, object]:
    """Build the approved deterministic development/screen/untouched split."""
    by_name = {name: (name, ref, checksum) for name, ref, checksum in identities}
    missing = set(DEVELOPMENT_TASK_NAMES) - set(by_name)
    if missing:
        raise ValueError(f"task identities omit development tasks: {sorted(missing)!r}")
    if len(by_name) != len(identities):
        raise ValueError("task identities contain duplicate task names")

    development = [by_name[name] for name in DEVELOPMENT_TASK_NAMES]
    remaining = [item for item in identities if item[0] not in DEVELOPMENT_TASK_NAMES]
    remaining.sort(
        key=lambda item: (
            hashlib.sha256((STUDY_ID + "\0" + item[1]).encode()).digest(),
            item[1],
        )
    )

    def record(item: TaskIdentity) -> dict[str, str]:
        return {
            "task_name": item[0],
            "canonical_task_reference": item[1],
            "task_checksum": item[2],
        }

    splits = {
        "development": [record(item) for item in development],
        "screen": [record(item) for item in remaining[:20]],
        "untouched": [record(item) for item in remaining[20:]],
    }
    return {
        "schema_version": TASK_PARTITION_SCHEMA,
        "study_id": STUDY_ID,
        **splits,
        "split_sha256": {
            name: hashlib.sha256(canonical_body_bytes(records)).hexdigest()
            for name, records in splits.items()
        },
    }


def _validate_fields(
    value: dict[str, object], expected: set[str], *, label: str
) -> None:
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        raise ValueError(f"{label} fields differ: missing={missing!r}, extra={extra!r}")


def validate_task_partition(value: object) -> dict[str, object]:
    """Validate a task partition against its schema, digests, and frozen seed."""
    if not isinstance(value, dict):
        raise ValueError("task partition must be an object")
    _validate_fields(value, _PARTITION_FIELDS, label="task partition")
    if value["schema_version"] != TASK_PARTITION_SCHEMA:
        raise ValueError("task partition schema_version is not approved")
    if value["study_id"] != STUDY_ID:
        raise ValueError("task partition study_id is not approved")

    names: set[str] = set()
    references: set[str] = set()
    for split_name in _SPLIT_NAMES:
        records = value[split_name]
        if not isinstance(records, list):
            raise ValueError(f"task partition {split_name} must be an array")
        for index, record in enumerate(records):
            label = f"task partition {split_name}[{index}]"
            if not isinstance(record, dict):
                raise ValueError(f"{label} must be an object")
            _validate_fields(record, _RECORD_FIELDS, label=label)
            task_name = record["task_name"]
            reference = record["canonical_task_reference"]
            checksum = record["task_checksum"]
            if not isinstance(task_name, str) or not task_name:
                raise ValueError(f"{label} has an invalid task name")
            if task_name in names:
                raise ValueError(
                    f"task partition has duplicate task name {task_name!r}"
                )
            names.add(task_name)
            if not isinstance(reference, str) or not _TASK_REFERENCE_RE.fullmatch(
                reference
            ):
                raise ValueError(f"{label} has an invalid task reference")
            if reference in references:
                raise ValueError(
                    f"task partition has duplicate task reference {reference!r}"
                )
            references.add(reference)
            if not isinstance(checksum, str) or not _SHA256_RE.fullmatch(checksum):
                raise ValueError(f"{label} has an invalid task checksum")

    digests = value["split_sha256"]
    if not isinstance(digests, dict):
        raise ValueError("task partition split_sha256 must be an object")
    _validate_fields(digests, set(_SPLIT_NAMES), label="task partition split_sha256")
    for split_name in _SPLIT_NAMES:
        digest = digests[split_name]
        expected_digest = hashlib.sha256(
            canonical_body_bytes(value[split_name])
        ).hexdigest()
        if digest != expected_digest:
            raise ValueError(f"task partition {split_name} split digest is incorrect")

    expected = build_task_partition(TASK_IDENTITIES)
    if any(value[split_name] != expected[split_name] for split_name in _SPLIT_NAMES):
        raise ValueError("task partition splits do not equal the frozen seed")
    return value
