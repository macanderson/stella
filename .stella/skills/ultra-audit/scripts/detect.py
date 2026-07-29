#!/usr/bin/env python3
"""Detect a workspace's languages, its real verification gate, and how much fix authority
an auditing agent can safely be given.

The audit harness is language-agnostic; this is the file that makes that true. It answers
four questions the rest of the run depends on:

  1. What languages are here, and how big is each?  -> partitioning
  2. What is the gate, in its CACHE-DEFEATED form?  -> the baseline and the verify phase
  3. Can an agent's bad edit be caught before it ships?  -> fix authority (operations.md #5)
  4. Is there an LLM surface?  -> whether token_efficiency is scored or N/A

    python3 detect.py <root> [--out gate.json] [--quiet]

Writes gate.json and prints a human summary. Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys

# fd/rg respect .gitignore, which covers most of these; the explicit list keeps
# non-git trees (and vendored directories that are checked in) from being counted.
IGNORE_DIRS = {
    ".git", ".hg", ".svn", "target", "node_modules", "dist", "build", "out",
    ".next", ".nuxt", ".svelte-kit", ".turbo", ".venv", "venv", "env",
    "__pycache__", ".mypy_cache", ".ruff_cache", ".pytest_cache", ".tox",
    "vendor", "coverage", "htmlcov", ".gradle", "Pods", ".terraform",
    "site-packages", ".cargo", ".idea", ".vscode", "third_party", "generated",
}

# ext -> (language, is_source). Only source languages get partitioned to unit agents.
LANGS = {
    "rs": "Rust", "go": "Go", "ts": "TypeScript", "tsx": "TypeScript",
    "js": "JavaScript", "jsx": "JavaScript", "mjs": "JavaScript", "cjs": "JavaScript",
    "py": "Python", "rb": "Ruby", "java": "Java", "kt": "Kotlin", "kts": "Kotlin",
    "swift": "Swift", "c": "C", "h": "C", "cc": "C++", "cpp": "C++", "hpp": "C++",
    "cs": "C#", "php": "PHP", "ex": "Elixir", "exs": "Elixir", "scala": "Scala",
    "zig": "Zig", "lua": "Lua", "sh": "Shell", "bash": "Shell", "sql": "SQL",
}
# Generated or vendored files masquerading as source.
GENERATED_HINTS = re.compile(
    r"(\.min\.|\.d\.ts$|_pb2?\.py$|\.pb\.go$|/migrations?/.*\.sql$|"
    r"\.generated\.|\.gen\.(go|ts|rs)$|__generated__)"
)


def sh(cmd: str, cwd: str, timeout: int = 60) -> str:
    try:
        p = subprocess.run(cmd, cwd=cwd, shell=True, capture_output=True,
                           text=True, timeout=timeout)
        return p.stdout
    except Exception:
        return ""


def have(tool: str) -> bool:
    return shutil.which(tool) is not None


def list_files(root: str) -> list[str]:
    """Every candidate source file, gitignore-aware when fd is available."""
    exts = sorted(LANGS)
    if have("fd"):
        args = " ".join(f"-e {e}" for e in exts)
        excl = " ".join(f"-E '{d}'" for d in sorted(IGNORE_DIRS))
        out = sh(f"fd -t f {args} {excl} . .", root, timeout=180)
        rel = [l for l in out.splitlines() if l.strip()]
        if rel:
            return rel
    rel = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in IGNORE_DIRS and not d.startswith(".")]
        for fn in filenames:
            if fn.rsplit(".", 1)[-1] in LANGS:
                rel.append(os.path.relpath(os.path.join(dirpath, fn), root))
    return rel


def count_lines(root: str, rel: str) -> int:
    try:
        with open(os.path.join(root, rel), "rb") as fh:
            return fh.read().count(b"\n") + 1
    except Exception:
        return 0


def survey(root: str) -> tuple[dict, list[dict]]:
    """LOC per language, and the per-file inventory the partitioner reuses."""
    files = []
    per_lang: dict[str, dict] = {}
    for rel in list_files(root):
        generated = bool(GENERATED_HINTS.search("/" + rel))
        ext = rel.rsplit(".", 1)[-1]
        lang = LANGS.get(ext)
        if not lang:
            continue
        n = count_lines(root, rel)
        files.append({"path": rel, "lang": lang, "loc": n, "generated": generated})
        if generated:
            continue
        d = per_lang.setdefault(lang, {"language": lang, "files": 0, "loc": 0, "exts": []})
        d["files"] += 1
        d["loc"] += n
        if ext not in d["exts"]:
            d["exts"].append(ext)
    return per_lang, files


# ── gate detection ───────────────────────────────────────────────────────────
# Each command carries the form that DEFEATS THE CACHE. A cached lint run reports
# zero warnings whether or not there are any (operations.md #3), so the forced form
# is the only one that is evidence.

def node_pm(root: str) -> str:
    for lock, pm in (("pnpm-lock.yaml", "pnpm"), ("bun.lockb", "bun"),
                     ("yarn.lock", "yarn"), ("package-lock.json", "npm")):
        if os.path.exists(os.path.join(root, lock)):
            return pm
    return "npm"


def node_scripts(root: str) -> dict:
    try:
        with open(os.path.join(root, "package.json")) as fh:
            return json.load(fh).get("scripts") or {}
    except Exception:
        return {}


def ts_is_strict(root: str) -> bool:
    """A non-strict tsconfig is not a type gate worth granting fix authority behind."""
    for name in ("tsconfig.json", "tsconfig.base.json"):
        p = os.path.join(root, name)
        if not os.path.exists(p):
            continue
        try:
            raw = open(p).read()
            raw = re.sub(r"/\*.*?\*/", "", raw, flags=re.S)
            raw = re.sub(r"(^|\s)//.*$", "", raw, flags=re.M)
            raw = re.sub(r",(\s*[}\]])", r"\1", raw)
            cfg = json.loads(raw)
            if (cfg.get("compilerOptions") or {}).get("strict") is True:
                return True
        except Exception:
            # An unparseable tsconfig is not evidence of strictness.
            continue
    return False


def project_gate_target(root: str) -> tuple[str, str] | None:
    """A repo that ships its own gate target knows better than we do — prefer it."""
    for fn, runner in (("Makefile", "make"), ("makefile", "make"), ("justfile", "just"),
                       ("Justfile", "just")):
        p = os.path.join(root, fn)
        if not os.path.exists(p):
            continue
        try:
            body = open(p, errors="replace").read()
        except Exception:
            continue
        for target in ("gate", "ci", "check", "verify", "validate", "lint-test"):
            if re.search(rf"^\.?{target}\s*:", body, flags=re.M):
                return runner, target
    return None


def detect_toolchains(root: str, langs: dict) -> list[dict]:
    tc: list[dict] = []

    if os.path.exists(os.path.join(root, "Cargo.toml")):
        tc.append({
            "name": "cargo", "language": "Rust", "type_gate": True,
            "fmt_check": "cargo fmt --all -- --check",
            "fmt_write": "cargo fmt --all",
            # Diagnostics are suppressed on a cached build; touch the crate roots first.
            # Brace alternation, NOT two -g flags: `-g` is a mode switch, so `-g a -g b`
            # makes `b` a search path and silently drops every main.rs.
            "lint": "fd -t f -g '{lib,main}.rs' . | xargs touch && "
                    "cargo clippy --workspace --all-targets --all-features",
            "test": "cargo test --workspace --all-features",
            "docs": "cargo doc --workspace --no-deps",
            "cache_note": "clippy is silent on a cached build — the touch is load-bearing",
        })

    if os.path.exists(os.path.join(root, "package.json")):
        pm = node_pm(root)
        s = node_scripts(root)
        run = f"{pm} run" if pm != "bun" else "bun run"
        pick = lambda *names: next((n for n in names if n in s), None)
        fmt = pick("format:check", "fmt:check", "format", "fmt", "prettier")
        lint = pick("lint", "lint:all", "eslint")
        test = pick("test", "test:unit", "test:ci")
        types = pick("typecheck", "type-check", "tsc", "types")
        strict = ts_is_strict(root)
        tc.append({
            "name": pm, "language": "TypeScript" if "TypeScript" in langs else "JavaScript",
            "type_gate": bool(types or os.path.exists(os.path.join(root, "tsconfig.json"))) and strict,
            "fmt_check": f"{run} {fmt}" if fmt else None,
            "fmt_write": f"{run} {fmt}" if fmt and "check" not in fmt else None,
            # --cache reuses prior verdicts; strip it.
            "lint": f"{run} {lint} -- --no-cache" if lint else None,
            "typecheck": (f"{run} {types}" if types else
                          ("npx tsc --noEmit --incremental false"
                           if os.path.exists(os.path.join(root, "tsconfig.json")) else None)),
            "test": f"{run} {test}" if test else None,
            "cache_note": ("delete .tsbuildinfo / pass --incremental false; eslint --cache "
                           "reuses prior verdicts"),
            "strict_types": strict,
            "scripts_available": sorted(s),
        })

    if any(os.path.exists(os.path.join(root, f)) for f in
           ("pyproject.toml", "setup.py", "setup.cfg", "requirements.txt")):
        uv = os.path.exists(os.path.join(root, "uv.lock"))
        pre = "uv run " if uv else ""
        has = lambda name: bool(sh(f"rg -l --max-count 1 '{name}' pyproject.toml setup.cfg "
                                  f"requirements.txt 2>/dev/null", root).strip())
        tc.append({
            "name": "uv" if uv else "python", "language": "Python",
            "type_gate": has("mypy") or has("pyright"),
            "fmt_check": f"{pre}ruff format --check ." if has("ruff") else
                         (f"{pre}black --check ." if has("black") else None),
            "fmt_write": f"{pre}ruff format ." if has("ruff") else
                         (f"{pre}black ." if has("black") else None),
            "lint": f"{pre}ruff check --no-cache ." if has("ruff") else None,
            "typecheck": (f"{pre}mypy --cache-dir=/dev/null ." if has("mypy") else
                          (f"{pre}pyright" if has("pyright") else None)),
            "test": f"{pre}pytest -q --cache-clear",
            "cache_note": "ruff/mypy cache aggressively — --no-cache / --cache-dir=/dev/null",
        })

    if os.path.exists(os.path.join(root, "go.mod")):
        tc.append({
            "name": "go", "language": "Go", "type_gate": True,
            "fmt_check": "test -z \"$(gofmt -l .)\"", "fmt_write": "gofmt -w .",
            "lint": "go vet ./...", "test": "go test -count=1 ./...",
            "cache_note": "-count=1 defeats the test result cache",
        })

    for f, name in (("build.gradle", "gradle"), ("build.gradle.kts", "gradle"),
                    ("pom.xml", "maven")):
        if os.path.exists(os.path.join(root, f)):
            w = "./gradlew" if os.path.exists(os.path.join(root, "gradlew")) else name
            tc.append({
                "name": name, "language": "Java/Kotlin", "type_gate": True,
                "lint": f"{w} check --rerun-tasks" if name == "gradle" else "mvn -o verify",
                "test": f"{w} test --rerun-tasks" if name == "gradle" else "mvn -o test",
                "cache_note": "--rerun-tasks defeats the Gradle build cache and daemon",
            })
            break

    if os.path.exists(os.path.join(root, "Gemfile")):
        tc.append({"name": "bundler", "language": "Ruby", "type_gate": False,
                   "lint": "bundle exec rubocop --no-parallel --cache false",
                   "test": "bundle exec rspec", "cache_note": "rubocop caches by default"})

    if os.path.exists(os.path.join(root, "Package.swift")):
        tc.append({"name": "swiftpm", "language": "Swift", "type_gate": True,
                   "lint": "swift build -Xswiftc -warnings-as-errors",
                   "test": "swift test", "cache_note": ""})

    if os.path.exists(os.path.join(root, "CMakeLists.txt")):
        tc.append({"name": "cmake", "language": "C/C++", "type_gate": True,
                   "lint": "cmake --build build --clean-first",
                   "test": "ctest --test-dir build --output-on-failure",
                   "cache_note": "--clean-first forces a real compile"})

    return tc


def git_info(root: str) -> dict:
    inside = sh("git rev-parse --is-inside-work-tree", root).strip() == "true"
    if not inside:
        return {"is_git": False, "head": None, "dirty": None, "default_branch": None}
    return {
        "is_git": True,
        "head": sh("git rev-parse HEAD", root).strip() or None,
        "dirty": bool(sh("git status --porcelain", root).strip()),
        "default_branch": (sh("git symbolic-ref --quiet --short refs/remotes/origin/HEAD", root)
                           .strip().rsplit("/", 1)[-1] or None),
        "has_remote": bool(sh("git remote", root).strip()),
    }


LLM_SIGNALS = (r"@anthropic-ai|anthropic|claude-(opus|sonnet|haiku|fable)|openai|"
               r"@ai-sdk|langchain|generateText|streamText|ChatCompletion|"
               r"messages\.create|tool_use|system_prompt|prompt_tokens")


def llm_surface(root: str) -> dict:
    if not have("rg"):
        return {"present": None, "evidence": []}
    out = sh(f"rg -l --max-count 1 -e '{LLM_SIGNALS}' "
             f"-g '!*.lock' -g '!*.json' -g '!*.md' . 2>/dev/null | head -12", root, timeout=120)
    hits = [l for l in out.splitlines() if l.strip()]
    return {"present": bool(hits), "evidence": hits[:12]}


def test_presence(root: str) -> dict:
    if not have("rg"):
        return {"test_files": None}
    out = sh("fd -t f -g '*test*' -g '*spec*' -g '*_test.*' -E node_modules -E target . . "
             "2>/dev/null | head -400", root, timeout=120)
    n = len([l for l in out.splitlines() if l.strip()])
    return {"test_files": n}


def fix_authority(tcs: list[dict], tests: dict, git: dict) -> dict:
    """See operations.md #5. A bad edit is only safe where something catches it."""
    if not git.get("is_git"):
        return {"level": "none",
                "reason": "not a git repository — a bad fix cannot be reverted, so no agent "
                          "gets write authority. Run audit-only.",
                "allowed": []}
    typed = [t["name"] for t in tcs if t.get("type_gate")]
    if typed:
        return {"level": "full",
                "reason": f"a compile/type gate exists ({', '.join(typed)}), so a bad edit "
                          f"fails the Verify phase rather than shipping silently.",
                "allowed": ["doc comments", "naming", "dead-code removal",
                            "error-message quality", "comment accuracy",
                            "clarifying refactors local to one function"]}
    if (tests.get("test_files") or 0) >= 5:
        return {"level": "tested-files-only",
                "reason": f"no type gate, but {tests['test_files']} test files exist. Edits are "
                          f"confined to files the test suite actually exercises — verify the "
                          f"test covers the line before touching it.",
                "allowed": ["doc comments", "naming inside a tested file",
                            "dead-code removal", "error-message quality",
                            "comment accuracy"]}
    return {"level": "text-only",
            "reason": "no type gate and no meaningful test suite — a bad edit would ship "
                      "silently. Text and dead code only.",
            "allowed": ["doc comments", "comment accuracy", "error-message text",
                        "dead-code removal proven unreachable by search"]}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("root")
    ap.add_argument("--out", default="gate.json")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    root = os.path.abspath(a.root)
    if not os.path.isdir(root):
        sys.exit(f"not a directory: {root}")

    langs, files = survey(root)
    if not langs:
        sys.exit(f"no recognised source files under {root}")
    tcs = detect_toolchains(root, langs)
    git = git_info(root)
    tests = test_presence(root)
    own = project_gate_target(root)

    gate = {
        "root": root,
        "languages": sorted(langs.values(), key=lambda d: -d["loc"]),
        "total_loc": sum(d["loc"] for d in langs.values()),
        "total_files": sum(d["files"] for d in langs.values()),
        "toolchains": tcs,
        "project_gate": {"runner": own[0], "target": own[1],
                         "command": f"{own[0]} {own[1]}"} if own else None,
        "git": git,
        "tests": tests,
        "llm_surface": llm_surface(root),
        "fix_authority": fix_authority(tcs, tests, git),
        # Files, so the partitioner does not have to walk the tree again and the
        # coverage proof can be computed against the same inventory.
        "inventory": files,
    }
    with open(a.out, "w") as fh:
        json.dump(gate, fh, indent=1)

    if a.quiet:
        print(a.out)
        return

    print(f"{root}")
    print(f"{gate['total_files']:,} source files · {gate['total_loc']:,} lines\n")
    for d in gate["languages"][:8]:
        print(f"  {d['language']:<14} {d['loc']:>9,}  ({d['files']:,} files)")
    print(f"\ngit: ", end="")
    print(f"HEAD {git['head'][:9]}" + (" DIRTY" if git["dirty"] else " clean")
          if git["is_git"] else "NOT A GIT REPO")
    if own:
        print(f"project's own gate: {own[0]} {own[1]}   <-- prefer this over the "
              f"per-toolchain commands")
    for t in tcs:
        print(f"\n{t['name']} ({t['language']})  type_gate={t.get('type_gate')}")
        for k in ("fmt_check", "lint", "typecheck", "test", "docs"):
            if t.get(k):
                print(f"    {k:<10} {t[k]}")
        if t.get("cache_note"):
            print(f"    ! {t['cache_note']}")
    fa = gate["fix_authority"]
    print(f"\nfix authority: {fa['level'].upper()}\n    {fa['reason']}")
    ls = gate["llm_surface"]
    print(f"\nLLM surface: {ls['present']}"
          f"{'  (token_efficiency is scored)' if ls['present'] else '  (token_efficiency = N/A, score 0)'}")
    print(f"\n-> {a.out}")
    print("REVIEW the gate commands before using them. A wrong gate produces a "
          "confidently wrong baseline.")


if __name__ == "__main__":
    main()
