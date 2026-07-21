from __future__ import annotations

import copy
import hashlib

import pytest

from tb21_evidence_contract import (
    build_task_partition,
    canonical_body_bytes,
    canonical_file_bytes,
    parse_canonical_object,
    validate_task_partition,
)
from tb21_study_seed import TASK_IDENTITIES, task_set_sha256


def _refresh_split_digest(partition: dict[str, object], split: str) -> None:
    digests = partition["split_sha256"]
    assert isinstance(digests, dict)
    digests[split] = hashlib.sha256(canonical_body_bytes(partition[split])).hexdigest()


def test_real_seed_and_partition_are_frozen() -> None:
    assert len(TASK_IDENTITIES) == 89
    assert task_set_sha256(TASK_IDENTITIES) == (
        "7e495afe0a86eaf572be1c2da2b9929c24e502adc888e550385d915cc0125ece"
    )
    partition = build_task_partition(TASK_IDENTITIES)
    assert [
        len(partition[name]) for name in ("development", "screen", "untouched")
    ] == [10, 20, 59]
    assert partition["split_sha256"] == {
        "development": (
            "265ef7896a287493fd846b5835d8eecb83e0e1dd74036aebd4c8e603cf5d3105"
        ),
        "screen": ("48828ea2c4fab2b7791a1b4e76e7d764c18cc94efb631bc944325aa91ace9866"),
        "untouched": (
            "324cfb122eb8220b4f7a177a932f1af45e5e4948fc22c9294156477d157bc26e"
        ),
    }
    screen = partition["screen"]
    assert isinstance(screen, list)
    assert [item["task_name"] for item in screen] == [
        "extract-moves-from-video",
        "pytorch-model-recovery",
        "dna-assembly",
        "path-tracing-reverse",
        "extract-elf",
        "build-cython-ext",
        "polyglot-c-py",
        "sparql-university",
        "polyglot-rust-c",
        "sqlite-db-truncate",
        "password-recovery",
        "build-pmars",
        "qemu-startup",
        "largest-eigenval",
        "regex-chess",
        "model-extraction-relu-logits",
        "mailman",
        "git-multibranch",
        "nginx-request-logging",
        "protein-assembly",
    ]
    assert validate_task_partition(partition) == partition


def test_canonical_json_has_distinct_file_and_body_encodings() -> None:
    value = {"z": "café", "a": [1, True, None]}
    body = b'{"a":[1,true,null],"z":"caf\xc3\xa9"}'

    assert canonical_body_bytes(value) == body
    assert canonical_file_bytes(value) == body + b"\n"
    assert parse_canonical_object(body + b"\n", label="fixture") == value


@pytest.mark.parametrize(
    "raw",
    [
        b'{"a":1,"a":2}\n',
        b"[]\n",
        b'{"z":2,"a":1}\n',
        b'{"a":1}',
        b'{"a":NaN}\n',
        b'{"a":Infinity}\n',
        b'{"a":-Infinity}\n',
    ],
)
def test_strict_parser_rejects_ambiguous_or_noncanonical_json(raw: bytes) -> None:
    with pytest.raises(ValueError, match="fixture"):
        parse_canonical_object(raw, label="fixture")


@pytest.mark.parametrize("field", ["schema_version", "study_id", "development"])
def test_partition_rejects_missing_fields(field: str) -> None:
    partition = build_task_partition(TASK_IDENTITIES)
    del partition[field]

    with pytest.raises(ValueError):
        validate_task_partition(partition)


def test_partition_rejects_extra_top_level_and_record_fields() -> None:
    partition = build_task_partition(TASK_IDENTITIES)
    partition["unexpected"] = True
    with pytest.raises(ValueError):
        validate_task_partition(partition)

    partition = build_task_partition(TASK_IDENTITIES)
    development = partition["development"]
    assert isinstance(development, list)
    development[0]["unexpected"] = True
    _refresh_split_digest(partition, "development")
    with pytest.raises(ValueError):
        validate_task_partition(partition)


def test_partition_rejects_duplicate_task_names_and_references() -> None:
    partition = build_task_partition(TASK_IDENTITIES)
    development = partition["development"]
    assert isinstance(development, list)
    development[1]["task_name"] = development[0]["task_name"]
    _refresh_split_digest(partition, "development")
    with pytest.raises(ValueError, match="duplicate task name"):
        validate_task_partition(partition)

    partition = build_task_partition(TASK_IDENTITIES)
    development = partition["development"]
    assert isinstance(development, list)
    development[1]["canonical_task_reference"] = development[0][
        "canonical_task_reference"
    ]
    _refresh_split_digest(partition, "development")
    with pytest.raises(ValueError, match="duplicate task reference"):
        validate_task_partition(partition)


def test_partition_rejects_incorrect_split_digest() -> None:
    partition = build_task_partition(TASK_IDENTITIES)
    digests = partition["split_sha256"]
    assert isinstance(digests, dict)
    digests["screen"] = "0" * 64

    with pytest.raises(ValueError, match="split digest"):
        validate_task_partition(partition)


def test_partition_rejects_a_digest_consistent_nonfrozen_split() -> None:
    partition = copy.deepcopy(build_task_partition(TASK_IDENTITIES))
    untouched = partition["untouched"]
    assert isinstance(untouched, list)
    untouched[0]["task_checksum"] = "0" * 64
    _refresh_split_digest(partition, "untouched")

    with pytest.raises(ValueError, match="frozen seed"):
        validate_task_partition(partition)
