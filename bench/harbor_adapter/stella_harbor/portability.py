"""Prove a host-built SUT binary can execute inside a task container.

Why this exists
---------------
Every integrity check the benchmark already runs answers *"did the file arrive
intact"* — the SHA-256 the container computes is compared against the host's,
and it matches because it is the same file on both sides. Nothing answered
*"can this file run here"*. On 2026-07-31 a run was provisioned by a script
that bypassed ``bench/evidence/run/build_sut.sh``::

    cargo build --release --locked -p stella-cli --bin stella
    cp target/release/stella target/x86_64-unknown-linux-gnu/release/stella

That produced a glibc-2.35 binary (Ubuntu 22.04 build host) wearing the
portable build's filename, in the exact path ``env.sh`` exports as
``STELLA_BINARY``. Every digest check passed. The first thing to touch the
dynamic loader was ``stella --version`` — inside the container, per trial,
after the run had started — and it died::

    /usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6:
        version `GLIBC_2.34' not found (required by /usr/local/bin/stella)

Harbor recorded ``NonZeroAgentExitCodeError`` and the trial scored 0.0,
indistinguishable in the results from the agent failing the task.

What this module checks
-----------------------
The requirement a binary places on its C library is recorded in the ELF
``.gnu.version_r`` section: one ``Verneed`` per needed library, each carrying
``Vernaux`` entries naming the symbol versions (``GLIBC_2.34``) the binary
demands. That table *is* what the loader walks at ``exec`` time to decide
whether to abort. Reading it on the host runs the loader's check ahead of time,
before any container exists.

Parsing is done here rather than by shelling out to ``readelf``/``objdump``
because those are absent from a macOS development host — a guard that silently
no-ops on the machine most likely to hold a wrong-architecture build is not a
guard. Pure stdlib also keeps the regression test hermetic: no binutils, no
Docker, no cross-compiler.

The floor
---------
``build_sut.sh`` cross-compiles against ``x86_64-unknown-linux-gnu.2.17``.
glibc 2.17 is RHEL 7 era (2012) and runs essentially anywhere. The oldest base
image in Terminal-Bench 2.1 is ``debian:bullseye-slim`` (glibc 2.31), so the
2.17 floor clears the whole dataset with fourteen years of margin. Both the
floor and the target triple are exported by ``env.sh`` so the build and this
check can never disagree about what "portable" means.

Run standalone::

    python3 stella_harbor/portability.py /path/to/stella --max-glibc 2.17

Exit status is ``0`` portable, ``1`` not portable, ``2`` could not be
determined. Preflight treats every nonzero status as fatal: a binary whose
portability cannot be established has not been shown to be portable.
"""

from __future__ import annotations

import argparse
import json
import mmap
import re
import struct
import sys
from dataclasses import dataclass, field
from pathlib import Path

__all__ = [
    "DEFAULT_GLIBC_FLOOR",
    "DEFAULT_MACHINE",
    "BinaryIncompatibleWithContainerError",
    "BinaryPortabilityError",
    "LibcProfile",
    "LoaderFailure",
    "MalformedElfError",
    "NotAnElfError",
    "classify_loader_failure",
    "check_portability",
    "format_version",
    "libc_profile_from_bytes",
    "parse_glibc_floor",
    "probe_libc_profile",
    "raise_for_loader_failure",
    "read_libc_profile",
]

# The glibc floor `build_sut.sh` targets. Kept in sync by `env.sh`, which
# exports STELLA_GLIBC_FLOOR to both the build and this check.
DEFAULT_GLIBC_FLOOR = (2, 17)

# Task images publish linux/amd64 only, and `env.sh` pins
# DOCKER_DEFAULT_PLATFORM to match. An arm64 binary is unrunnable there for the
# same class of reason a too-new glibc is, and fails just as late.
DEFAULT_MACHINE = "x86-64"

_ELF_MAGIC = b"\x7fELF"
_EI_NIDENT = 16
_ELFCLASS32 = 1
_ELFCLASS64 = 2
_ELFDATA2LSB = 1
_ELFDATA2MSB = 2
_PT_INTERP = 3
_SHT_GNU_VERNEED = 0x6FFFFFFE

_MACHINE_NAMES = {
    3: "x86",
    8: "mips",
    40: "arm",
    62: "x86-64",
    183: "aarch64",
    243: "riscv",
}

# `GLIBC_2.34`, `GLIBC_2.2.5`. Deliberately anchored: a `GLIBC_*` token that is
# not a dotted version (`GLIBC_ABI_DT_RELR`, emitted by glibc 2.36+ toolchains)
# cannot be shown satisfiable by an older libc, so it is reported rather than
# silently dropped.
_GLIBC_VERSION_TOKEN = re.compile(r"^GLIBC_(\d+(?:\.\d+)*)$")

_MUSL_INTERPRETER = re.compile(r"ld-musl", re.IGNORECASE)


class BinaryPortabilityError(RuntimeError):
    """The binary's runtime requirements could not be established."""


class NotAnElfError(BinaryPortabilityError):
    """The file is not an ELF object — e.g. a Mach-O or a shell script."""


class MalformedElfError(BinaryPortabilityError):
    """The file claims to be ELF but its headers do not parse."""


class BinaryIncompatibleWithContainerError(RuntimeError):
    """The uploaded SUT binary could not execute in the task container.

    Deliberately not a ``NonZeroAgentExitCodeError``. The install probe
    (``stella --version``) is the first command to touch the container's
    dynamic loader, so a mis-provisioned binary fails there — before the agent
    has been asked to do anything. Harbor writes the exception class into
    ``result.json``, and on 2026-07-31 five trials carrying this failure were
    recorded under the same class as a genuine nonzero agent exit and scored
    0.0, arithmetically indistinguishable from Stella failing those tasks.

    Raising a distinct class keeps a provisioning defect attributable as one.
    """


@dataclass(frozen=True)
class LibcProfile:
    """What a binary demands of the C library where it will be executed."""

    name: str
    machine: str
    bits: int
    interpreter: str | None
    libc_flavor: str
    # Every `GLIBC_*` token in `.gnu.version_r`, in file order.
    glibc_tokens: tuple[str, ...] = ()
    # The parsed subset, highest last.
    glibc_versions: tuple[tuple[int, ...], ...] = ()
    # `GLIBC_*` tokens that are not dotted versions.
    unparsed_glibc_tokens: tuple[str, ...] = ()
    needed_versioned_libraries: tuple[str, ...] = ()

    @property
    def max_glibc(self) -> tuple[int, ...] | None:
        """The highest glibc version this binary cannot run without."""
        return max(self.glibc_versions) if self.glibc_versions else None

    def describe(self) -> str:
        """One human-readable line naming the properties that decide the verdict."""
        parts = [f"{self.machine}/{self.bits}-bit", self.libc_flavor]
        highest = self.max_glibc
        if highest is not None:
            parts.append(f"max GLIBC_{format_version(highest)}")
        elif self.libc_flavor == "glibc":
            parts.append("no versioned glibc symbols")
        if self.interpreter:
            parts.append(f"interp {self.interpreter}")
        return ", ".join(parts)


@dataclass(frozen=True)
class LoaderFailure:
    """A container-side execution failure attributed to the binary, not the agent."""

    kind: str
    summary: str
    details: tuple[str, ...] = field(default=())


def format_version(version: tuple[int, ...]) -> str:
    """Render a parsed version tuple back to its dotted form."""
    return ".".join(str(part) for part in version)


def parse_glibc_floor(text: str) -> tuple[int, ...]:
    """Parse a dotted glibc floor such as ``2.17``."""
    stripped = text.strip()
    if not re.fullmatch(r"\d+(?:\.\d+)*", stripped):
        raise ValueError(f"not a dotted glibc version: {text!r}")
    return tuple(int(part) for part in stripped.split("."))


def _cstring(data: memoryview | bytes, start: int, limit: int) -> str:
    """Read a NUL-terminated string, refusing to run past ``limit``."""
    if start < 0 or start >= limit:
        raise MalformedElfError("string table offset is outside the section")
    end = start
    while end < limit and data[end] != 0:
        end += 1
    return bytes(data[start:end]).decode("utf-8", "replace")


def _unpack(fmt: str, data: memoryview | bytes, offset: int) -> tuple[int, ...]:
    """`struct.unpack_from` that reports a malformed ELF instead of a struct error."""
    try:
        return struct.unpack_from(fmt, data, offset)
    except struct.error as error:
        raise MalformedElfError(
            f"truncated ELF structure at offset {offset}"
        ) from error


def libc_profile_from_bytes(data: bytes | memoryview, *, name: str) -> LibcProfile:
    """Read the C-library requirements recorded in an ELF image."""
    if len(data) < _EI_NIDENT or bytes(data[:4]) != _ELF_MAGIC:
        raise NotAnElfError(
            f"{name} is not an ELF binary (no \\x7fELF magic) — a Linux task "
            "container cannot execute it whatever else is true of it"
        )

    ei_class = data[4]
    ei_data = data[5]
    if ei_class == _ELFCLASS64:
        bits, header_fmt, phdr_fmt, shdr_fmt = 64, "HHIQQQIHHHHHH", "IIQQQQ", "IIQQQQII"
    elif ei_class == _ELFCLASS32:
        bits, header_fmt, phdr_fmt, shdr_fmt = 32, "HHIIIIIHHHHHH", "IIIII", "IIIIIIII"
    else:
        raise MalformedElfError(f"{name}: unknown ELF class {ei_class}")

    if ei_data == _ELFDATA2LSB:
        endian = "<"
    elif ei_data == _ELFDATA2MSB:
        endian = ">"
    else:
        raise MalformedElfError(f"{name}: unknown ELF endianness {ei_data}")

    (
        _e_type,
        e_machine,
        _e_version,
        _e_entry,
        e_phoff,
        e_shoff,
        _e_flags,
        _e_ehsize,
        e_phentsize,
        e_phnum,
        e_shentsize,
        e_shnum,
        _e_shstrndx,
    ) = _unpack(endian + header_fmt, data, _EI_NIDENT)

    machine = _MACHINE_NAMES.get(e_machine, f"unknown({e_machine})")
    interpreter = _read_interpreter(
        data, endian + phdr_fmt, e_phoff, e_phentsize, e_phnum, bits
    )
    tokens, libraries = _read_version_requirements(
        data, endian + shdr_fmt, e_shoff, e_shentsize, e_shnum, endian
    )

    glibc_tokens = tuple(token for token in tokens if token.startswith("GLIBC_"))
    versions: list[tuple[int, ...]] = []
    unparsed: list[str] = []
    for token in glibc_tokens:
        match = _GLIBC_VERSION_TOKEN.match(token)
        if match is None:
            unparsed.append(token)
        else:
            versions.append(tuple(int(part) for part in match.group(1).split(".")))

    if interpreter and _MUSL_INTERPRETER.search(interpreter):
        flavor = "musl"
    elif glibc_tokens or (interpreter and "ld-linux" in interpreter):
        flavor = "glibc"
    elif interpreter is None:
        flavor = "static"
    else:
        flavor = "unknown"

    return LibcProfile(
        name=name,
        machine=machine,
        bits=bits,
        interpreter=interpreter,
        libc_flavor=flavor,
        glibc_tokens=glibc_tokens,
        glibc_versions=tuple(sorted(set(versions))),
        unparsed_glibc_tokens=tuple(dict.fromkeys(unparsed)),
        needed_versioned_libraries=libraries,
    )


def _read_interpreter(
    data: bytes | memoryview,
    fmt: str,
    phoff: int,
    phentsize: int,
    phnum: int,
    bits: int,
) -> str | None:
    """Return the ``PT_INTERP`` path — the loader this binary asks for by name."""
    if phoff == 0 or phnum == 0:
        return None
    for index in range(phnum):
        fields = _unpack(fmt, data, phoff + index * phentsize)
        if bits == 64:
            p_type, _p_flags, p_offset, _p_vaddr, _p_paddr, p_filesz = fields
        else:
            p_type, p_offset, _p_vaddr, _p_paddr, p_filesz = fields
        if p_type != _PT_INTERP:
            continue
        if p_offset + p_filesz > len(data):
            raise MalformedElfError("PT_INTERP segment runs past end of file")
        raw = bytes(data[p_offset : p_offset + p_filesz])
        return raw.split(b"\0", 1)[0].decode("utf-8", "replace")
    return None


def _read_version_requirements(
    data: bytes | memoryview,
    fmt: str,
    shoff: int,
    shentsize: int,
    shnum: int,
    endian: str,
) -> tuple[tuple[str, ...], tuple[str, ...]]:
    """Collect every symbol version named in ``.gnu.version_r``.

    Returns the version tokens (``GLIBC_2.34``) and the libraries requiring
    them (``libc.so.6``). A statically linked binary has no such section and
    correctly yields nothing.
    """
    if shoff == 0 or shnum == 0:
        return (), ()

    sections = []
    for index in range(shnum):
        fields = _unpack(fmt, data, shoff + index * shentsize)
        # sh_name, sh_type, sh_flags, sh_addr, sh_offset, sh_size, sh_link, sh_info
        sections.append((fields[1], fields[4], fields[5], fields[6], fields[7]))

    tokens: list[str] = []
    libraries: list[str] = []
    for sh_type, sh_offset, sh_size, sh_link, sh_info in sections:
        if sh_type != _SHT_GNU_VERNEED:
            continue
        if sh_link >= len(sections):
            raise MalformedElfError(".gnu.version_r links to a missing string table")
        _, str_offset, str_size, _, _ = sections[sh_link]
        str_limit = str_offset + str_size
        if str_limit > len(data) or sh_offset + sh_size > len(data):
            raise MalformedElfError(".gnu.version_r section runs past end of file")

        position = sh_offset
        section_end = sh_offset + sh_size
        for _ in range(sh_info):
            if position + 16 > section_end:
                raise MalformedElfError("Verneed entry runs past its section")
            _vn_version, vn_cnt, vn_file, vn_aux, vn_next = _unpack(
                endian + "HHIII", data, position
            )
            libraries.append(_cstring(data, str_offset + vn_file, str_limit))
            aux = position + vn_aux
            for _ in range(vn_cnt):
                if aux + 16 > section_end:
                    raise MalformedElfError("Vernaux entry runs past its section")
                _hash, _flags, _other, vna_name, vna_next = _unpack(
                    endian + "IHHII", data, aux
                )
                tokens.append(_cstring(data, str_offset + vna_name, str_limit))
                if vna_next == 0:
                    break
                aux += vna_next
            if vn_next == 0:
                break
            position += vn_next

    return tuple(tokens), tuple(dict.fromkeys(libraries))


def read_libc_profile(path: Path | str) -> LibcProfile:
    """Read a binary's C-library requirements from disk.

    ``mmap`` rather than ``read_bytes`` because the SUT is ~32 MiB and several
    trials can be diagnosing a failure at once; only the pages actually touched
    — the headers and two small sections — are ever resident.
    """
    binary = Path(path)
    with binary.open("rb") as handle:
        try:
            data = mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ)
        except ValueError as error:  # mmap of an empty file
            raise NotAnElfError(f"{binary} is empty") from error
        # Release the view before closing the map. A live memoryview makes
        # `mmap.close()` raise BufferError, which on the error path would
        # replace the diagnosis with an unrelated exception and defeat the
        # fail-closed exit status.
        view = memoryview(data)
        try:
            return libc_profile_from_bytes(view, name=str(binary))
        finally:
            view.release()
            data.close()


def check_portability(
    profile: LibcProfile,
    *,
    max_glibc: tuple[int, ...] = DEFAULT_GLIBC_FLOOR,
    machine: str | None = DEFAULT_MACHINE,
) -> list[str]:
    """Return every reason this binary would fail to execute in a task container.

    An empty list means the binary satisfies the floor. musl and fully static
    builds satisfy it trivially — they carry no glibc requirement at all — and
    are reported as such rather than treated as suspicious.
    """
    violations: list[str] = []

    if machine is not None and profile.machine != machine:
        violations.append(
            f"built for {profile.machine}, but task containers run {machine} "
            "(DOCKER_DEFAULT_PLATFORM=linux/amd64); it would die with "
            "'Exec format error'"
        )

    highest = profile.max_glibc
    if highest is not None and highest > max_glibc:
        offenders = sorted(
            (version for version in profile.glibc_versions if version > max_glibc),
            reverse=True,
        )
        named = ", ".join(f"GLIBC_{format_version(item)}" for item in offenders)
        violations.append(
            f"requires {named}, above the GLIBC_{format_version(max_glibc)} floor "
            "build_sut.sh targets — this is a host-glibc build, not the portable one"
        )

    if profile.unparsed_glibc_tokens:
        named = ", ".join(profile.unparsed_glibc_tokens)
        violations.append(
            f"requires the non-versioned glibc marker(s) {named}, which no glibc "
            f"at the GLIBC_{format_version(max_glibc)} floor provides"
        )

    return violations


def classify_loader_failure(text: str) -> LoaderFailure | None:
    """Recognise a dynamic-loader rejection in container output.

    The loader's own diagnostics are the only evidence available once the
    binary is already in the container, and each maps to a distinct provisioning
    mistake. Returning a typed failure lets the caller raise something that can
    never be tallied as the agent failing its task.
    """
    if not text:
        return None

    missing_versions = re.findall(
        r"version [`'\"]([A-Za-z0-9_.]+)['\"] not found", text
    )
    if missing_versions:
        unique = tuple(dict.fromkeys(missing_versions))
        return LoaderFailure(
            kind="glibc-too-old-for-binary",
            summary=(
                "the uploaded binary requires newer glibc symbol versions than "
                "the task container provides"
            ),
            details=unique,
        )

    if re.search(r"cannot execute binary file: Exec format error", text):
        return LoaderFailure(
            kind="wrong-architecture",
            summary="the uploaded binary was built for a different CPU architecture",
        )

    missing_libraries = re.findall(
        r"error while loading shared libraries: ([^:]+): cannot open shared object",
        text,
    )
    if missing_libraries:
        return LoaderFailure(
            kind="missing-shared-library",
            summary=(
                "the task container lacks a shared library the binary links against"
            ),
            details=tuple(dict.fromkeys(missing_libraries)),
        )

    if re.search(
        r"(?:not found|No such file or directory)\s*$", text.strip()
    ) and re.search(r"ld-(?:linux|musl)|/usr/local/bin/stella", text):
        return LoaderFailure(
            kind="loader-absent",
            summary=(
                "the task container has no dynamic loader for this binary "
                "(a glibc build on a musl image reports exactly this)"
            ),
        )

    return None


def probe_libc_profile(path: Path | str) -> LibcProfile | None:
    """Profile a binary for diagnosis, returning ``None`` if it cannot be read.

    The *blocking* check belongs on the host before a run starts
    (``assert_portable_binary`` in ``bench/evidence/run/env.sh``), where one
    message replaces an entire wasted run; a check that only fires per-trial has
    already cost what it was meant to save. This exists so that when a container
    does reject the binary, the error can report what was actually uploaded, and
    so it must tolerate anything it is handed — development builds, a Mach-O
    binary from a macOS tree, the short byte strings the adapter's tests use.
    """
    try:
        return read_libc_profile(path)
    except Exception:  # noqa: BLE001 - diagnostic aid, never fatal
        return None


def raise_for_loader_failure(error: Exception, binary: Path | str) -> None:
    """Re-raise a dynamic-loader rejection under its own class, or return.

    A genuine nonzero exit from the install probe (a full disk, a read-only
    ``/usr/local/bin``) is left alone to propagate unchanged. Only output
    carrying a loader diagnostic is reclassified, so this can never mask an
    unrelated failure as a packaging one. The host binary is profiled here
    rather than at install time because it is read only to compose this
    message — on a healthy run nothing ever parses it.
    """
    failure = classify_loader_failure(str(error))
    if failure is None:
        return
    detail = f" ({', '.join(failure.details)})" if failure.details else ""
    profile = probe_libc_profile(binary)
    uploaded = (
        f" The host binary that was uploaded is: {profile.describe()}."
        if profile
        else ""
    )
    raise BinaryIncompatibleWithContainerError(
        f"[{failure.kind}] {failure.summary}{detail}.{uploaded} This is a "
        "provisioning failure, not the agent failing the task — the install "
        "probe never returned, so Stella was never asked to do any work and "
        "this trial carries no signal about it. Rebuild the SUT with "
        "bench/evidence/run/build_sut.sh, which cross-compiles against the "
        "pinned glibc floor; a plain `cargo build --release` artifact copied "
        "into the target/ path reproduces exactly this while passing every "
        "digest check."
    ) from error


def _render_report(profile: LibcProfile, violations: list[str]) -> str:
    lines = [f"binary: {profile.name}", f"profile: {profile.describe()}"]
    if profile.needed_versioned_libraries:
        lines.append(f"versioned deps: {', '.join(profile.needed_versioned_libraries)}")
    if violations:
        lines.append("NOT PORTABLE:")
        lines.extend(f"  - {item}" for item in violations)
        lines.append(
            "Rebuild with `bench/evidence/run/build_sut.sh`, which cross-compiles "
            "against the pinned glibc floor via cargo-zigbuild. Do not "
            "`cargo build --release` and copy the result into the target/ path — "
            "that is the substitution this check exists to catch."
        )
    else:
        lines.append(
            "portable: satisfies the floor for every Terminal-Bench task image"
        )
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    """CLI entry point. 0 portable, 1 not portable, 2 undeterminable."""
    parser = argparse.ArgumentParser(
        prog="portability",
        description=(
            "Assert a SUT binary can execute in a Terminal-Bench task container "
            "before any container is created."
        ),
    )
    parser.add_argument("binary", help="path to the binary to inspect")
    parser.add_argument(
        "--max-glibc",
        default=format_version(DEFAULT_GLIBC_FLOOR),
        help="highest glibc version the binary may require (default: %(default)s)",
    )
    parser.add_argument(
        "--machine",
        default=DEFAULT_MACHINE,
        help="expected ELF machine, or 'any' to skip (default: %(default)s)",
    )
    parser.add_argument(
        "--json", action="store_true", help="emit the profile and verdict as JSON"
    )
    args = parser.parse_args(argv)

    try:
        floor = parse_glibc_floor(args.max_glibc)
    except ValueError as error:
        print(f"FATAL: {error}", file=sys.stderr)
        return 2

    try:
        profile = read_libc_profile(args.binary)
    except (BinaryPortabilityError, OSError) as error:
        # Fail closed: a binary whose requirements cannot be read has not been
        # shown to be runnable, and this check exists precisely because
        # "it looked fine" was how the bad binary got through.
        print(f"FATAL: cannot establish portability: {error}", file=sys.stderr)
        return 2

    machine = None if args.machine.lower() == "any" else args.machine
    violations = check_portability(profile, max_glibc=floor, machine=machine)

    if args.json:
        print(
            json.dumps(
                {
                    "binary": profile.name,
                    "machine": profile.machine,
                    "bits": profile.bits,
                    "libc_flavor": profile.libc_flavor,
                    "interpreter": profile.interpreter,
                    "glibc_tokens": list(profile.glibc_tokens),
                    "max_glibc": (
                        format_version(profile.max_glibc)
                        if profile.max_glibc is not None
                        else None
                    ),
                    "max_glibc_allowed": format_version(floor),
                    "violations": violations,
                    "portable": not violations,
                },
                indent=2,
            )
        )
    else:
        stream = sys.stderr if violations else sys.stdout
        print(_render_report(profile, violations), file=stream)

    return 1 if violations else 0


if __name__ == "__main__":  # pragma: no cover - exercised via subprocess in tests
    raise SystemExit(main())
