"""Regression tests for the SUT binary portability guard.

These would have caught the 2026-07-31 substitution. A run was provisioned with
``cargo build --release`` output copied into the path ``env.sh`` exports as
``STELLA_BINARY``; the resulting glibc-2.35 binary passed every existing check
(the host/container SHA-256 matched — it was the same file) and died in the
container on ``stella --version`` with ``GLIBC_2.34 not found``. Harbor scored
those trials 0.0, indistinguishable from the agent failing the task.

Every assertion here is on the *artifact*: an ELF image is synthesized with a
chosen set of glibc requirements and the guard's verdict is checked. Nothing
asserts that a particular build command was typed, because the failure was
precisely that the right command was not typed and everything downstream still
agreed. Synthesizing ELF bytes keeps the suite hermetic — no binutils, no
Docker, no cross-compiler — so it runs identically on a macOS laptop and in CI.

The parser is additionally validated against both real artifacts: the exact
binary that broke the run (rejected, max ``GLIBC_2.34``) and a genuine
``cargo zigbuild --target x86_64-unknown-linux-gnu.2.17`` build (accepted, max
``GLIBC_2.17``). Those cannot be committed — one is 32 MiB — so they are
exercised through the opt-in fixture test at the bottom.

``stella_harbor/portability.py`` is loaded by path, the same way
``test_git_baseline.py`` loads its subject: the module is stdlib-only on
purpose — ``env.sh`` invokes it by file path on a host that is only building
the SUT — and importing it through the package would pull in the adapter
root, and with it ``harbor``. A module that houses
``test_runs_without_importing_the_adapter_package`` must not itself be
uncollectable without the package.
"""

from __future__ import annotations

import importlib.util
import struct
import subprocess
import sys
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[3]
_CHECKER = _REPO_ROOT / "bench" / "harbor_adapter" / "stella_harbor" / "portability.py"
_ENV_SH = _REPO_ROOT / "bench" / "evidence" / "run" / "env.sh"

_spec = importlib.util.spec_from_file_location("stella_harbor_portability", _CHECKER)
# Module-scope assert, deliberately: an import precondition, not a constant
# check — without it there is nothing to collect (see #1452's sweep).
assert _spec is not None and _spec.loader is not None
_portability = importlib.util.module_from_spec(_spec)
# Registered before execution: `@dataclass` resolves a class's own module out
# of `sys.modules` while the decorator runs, and a module that is not there
# yet fails the whole load.
sys.modules[_spec.name] = _portability
_spec.loader.exec_module(_portability)

DEFAULT_GLIBC_FLOOR = _portability.DEFAULT_GLIBC_FLOOR
LibcProfile = _portability.LibcProfile
MalformedElfError = _portability.MalformedElfError
NotAnElfError = _portability.NotAnElfError
check_portability = _portability.check_portability
classify_loader_failure = _portability.classify_loader_failure
libc_profile_from_bytes = _portability.libc_profile_from_bytes
parse_glibc_floor = _portability.parse_glibc_floor
read_libc_profile = _portability.read_libc_profile

_EM_X86_64 = 62
_EM_AARCH64 = 183
_SHT_STRTAB = 3
_SHT_GNU_VERNEED = 0x6FFFFFFE
_PT_INTERP = 3

_GLIBC_LOADER = "/lib64/ld-linux-x86-64.so.2"
_MUSL_LOADER = "/lib/ld-musl-x86_64.so.1"

# The literal container output from
# rig-runs/jobs/dev2-armA-stella/qemu-alpine-ssh__Q2td85H/exception.txt.
_OBSERVED_LOADER_REJECTION = (
    "Command failed (exit 1): sha256sum /tmp/stella-upload && cp "
    "/tmp/stella-upload /usr/local/bin/stella && chmod +x /usr/local/bin/stella "
    "&& /usr/local/bin/stella --version\n"
    "stdout: 852f8cfd308ee063e3ad46b556483ae1fd9c591cfc89a0d9d2defbe51eef3687  "
    "/tmp/stella-upload\n"
    "/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version "
    "`GLIBC_2.32' not found (required by /usr/local/bin/stella)\n"
    "/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version "
    "`GLIBC_2.33' not found (required by /usr/local/bin/stella)\n"
    "/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version "
    "`GLIBC_2.34' not found (required by /usr/local/bin/stella)\n\n"
    "stderr: None"
)


def build_elf(
    *,
    machine: int = _EM_X86_64,
    interpreter: str | None = _GLIBC_LOADER,
    requirements: dict[str, list[str]] | None = None,
    endian: str = "<",
) -> bytes:
    """Assemble a minimal but structurally valid ELF64 image.

    Carries only what decides the verdict: the machine, the ``PT_INTERP``
    loader path, and a ``.gnu.version_r`` table naming the symbol versions each
    shared library must provide. That is the same table the dynamic loader
    consults at ``exec``, so a binary built here fails or passes the guard for
    exactly the reason a real one would.
    """
    ehsize, phentsize, shentsize = 64, 56, 64

    dynstr = bytearray(b"\0")
    offsets: dict[str, int] = {}

    def intern(text: str) -> int:
        if text not in offsets:
            offsets[text] = len(dynstr)
            dynstr.extend(text.encode() + b"\0")
        return offsets[text]

    entries = list((requirements or {}).items())
    for library, versions in entries:
        intern(library)
        for version in versions:
            intern(version)

    verneed = bytearray()
    for index, (library, versions) in enumerate(entries):
        last_library = index == len(entries) - 1
        verneed += struct.pack(
            endian + "HHIII",
            1,  # vn_version
            len(versions),  # vn_cnt
            intern(library),  # vn_file
            16,  # vn_aux — first Vernaux follows immediately
            0 if last_library else 16 + 16 * len(versions),  # vn_next
        )
        for position, version in enumerate(versions):
            last_version = position == len(versions) - 1
            verneed += struct.pack(
                endian + "IHHII",
                0,  # vna_hash
                0,  # vna_flags
                position + 2,  # vna_other
                intern(version),  # vna_name
                0 if last_version else 16,  # vna_next
            )

    phnum = 1 if interpreter else 0
    cursor = ehsize + phentsize * phnum
    interp_offset = cursor
    interp_bytes = interpreter.encode() + b"\0" if interpreter else b""
    cursor += len(interp_bytes)
    dynstr_offset = cursor
    cursor += len(dynstr)
    verneed_offset = cursor
    cursor += len(verneed)
    padding = (-cursor) % 8
    cursor += padding
    shoff = cursor
    shnum = 3 if entries else 2

    image = bytearray()
    image += b"\x7fELF" + bytes([2, 1 if endian == "<" else 2, 1, 0, 0]) + b"\0" * 7
    image += struct.pack(
        endian + "HHIQQQIHHHHHH",
        3,  # e_type = ET_DYN
        machine,
        1,  # e_version
        0,  # e_entry
        ehsize if phnum else 0,  # e_phoff
        shoff,
        0,  # e_flags
        ehsize,
        phentsize,
        phnum,
        shentsize,
        shnum,
        0,  # e_shstrndx — sections are found by type, never by name
    )
    if phnum:
        image += struct.pack(
            endian + "IIQQQQQQ",
            _PT_INTERP,
            4,  # p_flags = PF_R
            interp_offset,
            0,
            0,
            len(interp_bytes),
            len(interp_bytes),
            1,
        )
    image += interp_bytes
    image += dynstr
    image += verneed
    image += b"\0" * padding

    def section(
        sh_type: int, offset: int, size: int, link: int = 0, info: int = 0
    ) -> bytes:
        return struct.pack(
            endian + "IIQQQQIIQQ", 0, sh_type, 0, 0, offset, size, link, info, 1, 0
        )

    image += section(0, 0, 0)
    image += section(_SHT_STRTAB, dynstr_offset, len(dynstr))
    if entries:
        image += section(
            _SHT_GNU_VERNEED, verneed_offset, len(verneed), link=1, info=len(entries)
        )
    return bytes(image)


def profile_of(**kwargs: object) -> LibcProfile:
    """Synthesize an ELF and read back its libc requirements."""
    return libc_profile_from_bytes(build_elf(**kwargs), name="synthetic")  # type: ignore[arg-type]


class TestTheSubstitutedBinary:
    """The exact defect: a host-glibc build wearing the portable build's name."""

    def test_host_glibc_build_is_rejected_with_the_offending_symbols_named(
        self,
    ) -> None:
        profile = profile_of(
            requirements={
                "libc.so.6": [
                    "GLIBC_2.2.5",
                    "GLIBC_2.14",
                    "GLIBC_2.32",
                    "GLIBC_2.33",
                    "GLIBC_2.34",
                ]
            }
        )
        assert profile.max_glibc == (2, 34)

        violations = check_portability(profile)
        assert violations, "a glibc-2.34 binary must not pass the 2.17 floor"
        message = " ".join(violations)
        # The message has to name the symbol so an operator can act on it.
        assert "GLIBC_2.34" in message
        assert "GLIBC_2.17" in message
        assert "GLIBC_2.14" not in message, "symbols under the floor are not offenders"

    def test_portable_build_at_the_floor_is_accepted(self) -> None:
        # A real `cargo zigbuild --target x86_64-unknown-linux-gnu.2.17` build
        # requires exactly up to GLIBC_2.17, so the comparison must be
        # inclusive. An exclusive bound would reject every correct build.
        profile = profile_of(
            requirements={
                "libc.so.6": ["GLIBC_2.2.5", "GLIBC_2.14", "GLIBC_2.16", "GLIBC_2.17"]
            }
        )
        assert profile.max_glibc == DEFAULT_GLIBC_FLOOR
        assert check_portability(profile) == []

    def test_versions_are_ordered_numerically_not_lexically(self) -> None:
        # "2.9" sorts above "2.17" as text. A string comparison here would
        # reject a correct build and, worse, could accept a bad one.
        profile = profile_of(requirements={"libc.so.6": ["GLIBC_2.9", "GLIBC_2.2.5"]})
        assert profile.max_glibc == (2, 9)
        assert check_portability(profile) == []

    def test_dotted_patch_versions_parse(self) -> None:
        profile = profile_of(requirements={"libc.so.6": ["GLIBC_2.3.4"]})
        assert profile.max_glibc == (2, 3, 4)
        assert check_portability(profile) == []

    def test_multiple_versioned_libraries_are_all_considered(self) -> None:
        profile = profile_of(
            requirements={
                "libc.so.6": ["GLIBC_2.17"],
                "libm.so.6": ["GLIBC_2.29"],
                "libgcc_s.so.1": ["GCC_3.0"],
            }
        )
        assert profile.needed_versioned_libraries == (
            "libc.so.6",
            "libm.so.6",
            "libgcc_s.so.1",
        )
        # Non-glibc version tokens are irrelevant to the floor and ignored.
        assert "GCC_3.0" not in profile.glibc_tokens
        assert profile.max_glibc == (2, 29)
        assert check_portability(profile)


class TestOtherWaysABinaryCannotRunThere:
    def test_wrong_architecture_is_rejected(self) -> None:
        # env.sh pins DOCKER_DEFAULT_PLATFORM=linux/amd64 so a multi-arch base
        # cannot build arm64 and then fail to exec it. That intent is only
        # enforceable by looking at the artifact.
        profile = profile_of(machine=_EM_AARCH64, requirements={"libc.so.6": []})
        assert profile.machine == "aarch64"
        violations = check_portability(profile)
        assert any("aarch64" in item for item in violations)

    def test_architecture_check_can_be_waived(self) -> None:
        profile = profile_of(machine=_EM_AARCH64, requirements={"libc.so.6": []})
        assert check_portability(profile, machine=None) == []

    def test_non_elf_file_is_rejected(self) -> None:
        # A macOS development build in STELLA_BINARY is Mach-O; the old
        # `test -x` preflight accepted it and the failure surfaced per-trial.
        with pytest.raises(NotAnElfError):
            libc_profile_from_bytes(b"\xcf\xfa\xed\xfe" + b"\0" * 60, name="mach-o")

    def test_shell_script_is_rejected(self) -> None:
        with pytest.raises(NotAnElfError):
            libc_profile_from_bytes(b'#!/bin/sh\nexec stella "$@"\n', name="wrapper")

    def test_truncated_elf_is_rejected_rather_than_assumed_fine(self) -> None:
        image = build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]})
        with pytest.raises((MalformedElfError, NotAnElfError)):
            libc_profile_from_bytes(image[:80], name="truncated")

    def test_unknown_elf_class_is_rejected(self) -> None:
        broken = bytearray(build_elf(requirements={"libc.so.6": ["GLIBC_2.17"]}))
        broken[4] = 7  # neither ELFCLASS32 nor ELFCLASS64
        with pytest.raises(MalformedElfError):
            libc_profile_from_bytes(bytes(broken), name="bad-class")

    def test_non_versioned_glibc_marker_is_reported(self) -> None:
        # glibc 2.36+ toolchains can emit GLIBC_ABI_DT_RELR, which is not a
        # dotted version and cannot be shown satisfiable by an older libc.
        # Fail closed and name it rather than silently dropping the token.
        profile = profile_of(
            requirements={"libc.so.6": ["GLIBC_2.17", "GLIBC_ABI_DT_RELR"]}
        )
        assert profile.unparsed_glibc_tokens == ("GLIBC_ABI_DT_RELR",)
        violations = check_portability(profile)
        assert any("GLIBC_ABI_DT_RELR" in item for item in violations)


class TestBuildsWithNoGlibcRequirement:
    """musl and static builds clear the floor trivially — they carry no glibc."""

    def test_static_binary_is_portable(self) -> None:
        profile = profile_of(interpreter=None, requirements=None)
        assert profile.libc_flavor == "static"
        assert profile.max_glibc is None
        assert check_portability(profile) == []

    def test_musl_binary_is_portable(self) -> None:
        profile = profile_of(interpreter=_MUSL_LOADER, requirements=None)
        assert profile.libc_flavor == "musl"
        assert check_portability(profile) == []

    def test_glibc_flavor_is_detected_from_the_loader(self) -> None:
        profile = profile_of(requirements={"libc.so.6": ["GLIBC_2.17"]})
        assert profile.libc_flavor == "glibc"
        assert profile.interpreter == _GLIBC_LOADER


class TestLoaderFailureClassification:
    def test_the_observed_container_output_is_classified(self) -> None:
        failure = classify_loader_failure(_OBSERVED_LOADER_REJECTION)
        assert failure is not None
        assert failure.kind == "glibc-too-old-for-binary"
        assert failure.details == ("GLIBC_2.32", "GLIBC_2.33", "GLIBC_2.34")

    def test_exec_format_error_is_classified(self) -> None:
        failure = classify_loader_failure(
            "/usr/local/bin/stella: cannot execute binary file: Exec format error"
        )
        assert failure is not None
        assert failure.kind == "wrong-architecture"

    def test_missing_shared_library_is_classified(self) -> None:
        failure = classify_loader_failure(
            "/usr/local/bin/stella: error while loading shared libraries: "
            "libssl.so.3: cannot open shared object file: No such file or directory"
        )
        assert failure is not None
        assert failure.kind == "missing-shared-library"
        assert failure.details == ("libssl.so.3",)

    def test_absent_loader_is_classified(self) -> None:
        # A glibc binary on a musl image reports exactly this from the shell.
        failure = classify_loader_failure("/usr/local/bin/stella: not found")
        assert failure is not None
        assert failure.kind == "loader-absent"

    @pytest.mark.parametrize(
        "text",
        [
            "",
            "cp: cannot create regular file '/usr/local/bin/stella': "
            "Read-only file system",
            "sha256sum: /tmp/stella-upload: No space left on device",
            "stella 0.6.24",
        ],
    )
    def test_unrelated_failures_are_not_reclassified(self, text: str) -> None:
        # Reclassifying a genuine install failure as a packaging one would hide
        # it just as thoroughly as the bug being fixed.
        assert classify_loader_failure(text) is None


class TestFloorParsing:
    @pytest.mark.parametrize(
        ("text", "expected"),
        [("2.17", (2, 17)), ("2", (2,)), ("2.3.4", (2, 3, 4)), (" 2.17 ", (2, 17))],
    )
    def test_valid_floors(self, text: str, expected: tuple[int, ...]) -> None:
        assert parse_glibc_floor(text) == expected

    @pytest.mark.parametrize("text", ["", "2.x", "GLIBC_2.17", "2..17", "latest"])
    def test_invalid_floors_are_rejected(self, text: str) -> None:
        with pytest.raises(ValueError):
            parse_glibc_floor(text)


class TestCommandLine:
    """The CLI is what `env.sh` actually invokes, so its contract is tested."""

    @staticmethod
    def _run(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(_CHECKER), *args],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_exits_nonzero_and_names_the_symbol_for_a_bad_binary(
        self, tmp_path: Path
    ) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]}))
        result = self._run(str(binary))
        assert result.returncode == 1
        combined = result.stdout + result.stderr
        assert "GLIBC_2.34" in combined
        assert "build_sut.sh" in combined

    def test_exits_zero_for_a_portable_binary(self, tmp_path: Path) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.17"]}))
        assert self._run(str(binary)).returncode == 0

    def test_undeterminable_binary_exits_two_rather_than_zero(
        self, tmp_path: Path
    ) -> None:
        # Fail closed: "could not be established" is not "portable".
        binary = tmp_path / "stella"
        binary.write_bytes(b"not an elf at all")
        assert self._run(str(binary)).returncode == 2

    def test_empty_file_exits_two(self, tmp_path: Path) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(b"")
        assert self._run(str(binary)).returncode == 2

    def test_missing_file_exits_two(self, tmp_path: Path) -> None:
        assert self._run(str(tmp_path / "absent")).returncode == 2

    def test_invalid_floor_exits_two(self, tmp_path: Path) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.17"]}))
        assert self._run(str(binary), "--max-glibc", "nonsense").returncode == 2

    def test_json_report_is_machine_readable(self, tmp_path: Path) -> None:
        import json

        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]}))
        result = self._run(str(binary), "--json")
        payload = json.loads(result.stdout)
        assert payload["portable"] is False
        assert payload["max_glibc"] == "2.34"
        assert payload["max_glibc_allowed"] == "2.17"
        assert payload["machine"] == "x86-64"

    def test_runs_without_importing_the_adapter_package(self, tmp_path: Path) -> None:
        # `env.sh` invokes the checker by file path, deliberately: importing
        # `stella_harbor` pulls in Harbor, which need not be installed on a host
        # that is only building the SUT. Run it with the package unimportable.
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]}))
        result = subprocess.run(
            [sys.executable, "-S", str(_CHECKER), str(binary)],
            capture_output=True,
            text=True,
            cwd=tmp_path,
            env={"PATH": "/usr/bin:/bin", "PYTHONPATH": ""},
            check=False,
        )
        assert result.returncode == 1


class TestPreflightWiring:
    """The guard must be *called*, not merely present in the tree.

    A check nobody invokes is exactly as protective as no check. These tests
    fail if `assert_portable_binary` is removed from `env.sh`, or if `preflight`
    stops calling it.
    """

    @staticmethod
    def _bash(script: str, tb_root: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", "-c", script],
            capture_output=True,
            text=True,
            check=False,
            env={
                "PATH": "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
                "TB_REPO": str(_REPO_ROOT),
                "TB_ROOT": str(tb_root),
                "HOME": str(tb_root),
            },
        )

    def test_env_sh_rejects_a_non_portable_binary(self, tmp_path: Path) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]}))
        binary.chmod(0o755)
        result = self._bash(
            f'source "$TB_REPO/bench/evidence/run/env.sh"; '
            f'assert_portable_binary "{binary}"',
            tmp_path,
        )
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        assert "GLIBC_2.34" in combined
        assert "build_sut.sh" in combined

    def test_env_sh_accepts_a_portable_binary(self, tmp_path: Path) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.17"]}))
        binary.chmod(0o755)
        result = self._bash(
            f'source "$TB_REPO/bench/evidence/run/env.sh"; '
            f'assert_portable_binary "{binary}"',
            tmp_path,
        )
        assert result.returncode == 0, result.stdout + result.stderr

    def test_preflight_calls_the_guard_before_it_can_reach_docker(
        self, tmp_path: Path
    ) -> None:
        # preflight's later checks need Harbor and Docker. Asserting the run
        # dies on the binary proves the guard runs early enough to be reached
        # on a host where those are unavailable, and that it is wired in at all.
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]}))
        binary.chmod(0o755)
        result = self._bash(
            'source "$TB_REPO/bench/evidence/run/env.sh"; '
            f'export STELLA_BINARY="{binary}"; '
            "export OPENROUTER_API_KEY=test-key; "
            f'export STELLA_SOURCE_COMMIT="{"a" * 40}"; '
            "preflight",
            tmp_path,
        )
        assert result.returncode != 0
        assert "GLIBC_2.34" in result.stdout + result.stderr

    def test_the_floor_is_shared_by_the_build_and_the_check(self) -> None:
        # build_sut.sh must cross-compile against the same floor the guard
        # enforces. Two literals that can drift apart is the class of mistake
        # this whole change is about.
        env_text = _ENV_SH.read_text()
        build_text = (_REPO_ROOT / "bench/evidence/run/build_sut.sh").read_text()
        assert 'STELLA_GLIBC_FLOOR="2.17"' in env_text
        assert "$STELLA_TARGET_TRIPLE.$STELLA_GLIBC_FLOOR" in build_text
        assert "2.17" not in build_text, "the floor must not be duplicated as a literal"


@pytest.mark.skipif(
    not Path("/proc/self/exe").exists() and sys.platform != "darwin",
    reason="fixture test is opt-in on any platform",
)
class TestRealArtifacts:
    """Opt-in checks against binaries too large to commit.

    Point ``STELLA_PORTABILITY_FIXTURE`` at a real build to verify the parser
    against it. Used during development against the 32 MiB binary from the
    failed run (rejected, ``GLIBC_2.34``) and a genuine zigbuild-2.17 artifact
    (accepted, ``GLIBC_2.17``).
    """

    def test_real_binary_if_provided(self, monkeypatch: pytest.MonkeyPatch) -> None:
        import os

        fixture = os.environ.get("STELLA_PORTABILITY_FIXTURE")
        if not fixture:
            pytest.skip("set STELLA_PORTABILITY_FIXTURE to a real Linux binary")
        profile = read_libc_profile(fixture)
        assert profile.machine == "x86-64"
        expected = os.environ.get("STELLA_PORTABILITY_EXPECT", "portable")
        violations = check_portability(profile)
        if expected == "portable":
            assert violations == [], profile.describe()
        else:
            assert violations, profile.describe()
