//! Resource probes for the self-driving governor — cross-platform, best-effort,
//! never fatal. A probe degrades rather than failing: a governor that dies
//! because `pmset` is missing is worse than one that assumes mains power.
//!
//! # Which way each probe degrades, and why it is not one rule
//!
//! This used to say every probe degrades to a *conservative* value. That is
//! true of most and was never true of all, and `load1` was the one where the
//! gap bit: it fell back to `0`, an idle box, which cannot trip the
//! shed-to-Light branch and actively qualifies the escalate-to-Heavy one
//! (#5359). The least information bought the most expensive decision.
//!
//! - **Toward scarcity** — `cpu_total` (2), `mem_total_gb` (8),
//!   `mem_free_gb` (2), `disk_free_gb` (0, which trips the disk floor). An
//!   unmeasured box looks small, so it is not asked to do much.
//! - **Toward neither** — `load1`, which stands at half the core count: the
//!   one value that satisfies neither `load1 < cpu / 2` (Heavy) nor
//!   `load1 >= cpu` (Light), so an unmeasured load runs the ordinary cycle.
//! - **Toward permissive, on purpose** — `on_battery` (mains) and
//!   `contention` (free). Both would otherwise pin the loop to the light tier
//!   for the life of a box that simply lacks `pmset` or `pgrep`, and
//!   `contention`'s own doc names that failure: permanent, silent, and
//!   looking like caution.
//!
//! A reader adding a probe should say which of the three it takes and why,
//! rather than inheriting a blanket claim that cannot hold for all of them.
//!
//! And every probe yields to a `SELF_DRIVING_PROBE_*` override before touching
//! the machine, for two consumers. The hermetic test suites (Rust and shell)
//! have to drive the tier ladder on whatever box CI hands them. And an
//! operator on a box where a probe reads the wrong machine (a container
//! whose /proc reports the host, an overlay mount whose df reports the
//! layer) needs to pin the sense rather than fight the conclusion — a
//! governor whose senses cannot be pinned can only be believed.

use std::path::Path;
use std::process::Command;

use stella_autonomy::Supply;

fn env_override(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Core count. Honours `SELF_DRIVING_PROBE_CPU` like every other probe.
/// Crate-visible because the AIMD limits derive their hard cap from this
/// alone. Reading the whole supply there would pay for the battery and
/// busy-box probes just to drop them.
pub(crate) fn cpu_total() -> u32 {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_CPU") {
        return v as u32;
    }
    stella_autonomy::detected_cpus()
}

/// Integer part of the 1-minute load average; the governor compares
/// magnitudes, not decimals.
/// The 1-minute load average, or `None` when this box will not say.
///
/// `None` rather than `0`, which is what this returned and is the one value a
/// governor must never invent: 0 is an idle machine. It cannot trip the
/// `load1 >= cpu` shed-to-Light branch, and it actively QUALIFIES the
/// `load1 < cpu / 2` branch that escalates to Heavy — a full workspace build
/// and a head-to-head bench, on a box whose load nobody measured. The reason
/// string that branch prints even says "and idle".
///
/// A container without `uptime`, or one whose `uptime` spells its output
/// differently, is enough. See `supply` for what replaces the guess.
fn load1() -> Option<u32> {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_LOAD1") {
        return Some(v as u32);
    }
    if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
        return parse_leading_int(text.split_whitespace().next().unwrap_or(""));
    }
    // `uptime` tails with "load averages: 2.05 2.33 2.55" (macOS spells it
    // "averages", Linux "average:").
    run("uptime", &[]).and_then(|text| {
        let tail = text.rsplit("load average").next()?.to_string();
        let first = tail
            .trim_start_matches(|c: char| c == 's' || c == ':' || c.is_whitespace())
            .split([',', ' '])
            .next()?
            .to_string();
        parse_leading_int(&first)
    })
}

/// The integer part of a load figure, or `None` when the text is not one.
///
/// A malformed `/proc/loadavg` is a box that did not answer, not a box at
/// rest — the same distinction `load1` turns on.
fn parse_leading_int(text: &str) -> Option<u32> {
    text.split('.').next()?.trim().parse().ok()
}

fn mem_total_gb() -> u64 {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_MEM_TOTAL_GB") {
        return v;
    }
    if let Some(kb) = meminfo_kb("MemTotal:") {
        return kb / 1024 / 1024;
    }
    run("sysctl", &["-n", "hw.memsize"])
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|bytes| bytes / (1 << 30))
        .unwrap_or(8)
}

fn mem_free_gb() -> u64 {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_MEM_FREE_GB") {
        return v;
    }
    if let Some(kb) = meminfo_kb("MemAvailable:") {
        return kb / 1024 / 1024;
    }
    // macOS has no MemAvailable. Free + inactive + speculative is the
    // closest honest analogue: pages the kernel can hand out without
    // swapping.
    run("vm_stat", &[])
        .map(|text| {
            let mut page_size: u64 = 4096;
            let mut pages: u64 = 0;
            for line in text.lines() {
                if line.contains("page size of") {
                    page_size = line
                        .chars()
                        .filter(char::is_ascii_digit)
                        .collect::<String>()
                        .parse()
                        .unwrap_or(4096);
                }
                if line.starts_with("Pages free")
                    || line.starts_with("Pages inactive")
                    || line.starts_with("Pages speculative")
                {
                    let digits: String = line.chars().filter(char::is_ascii_digit).collect();
                    pages += digits.parse::<u64>().unwrap_or(0);
                }
            }
            page_size * pages / (1 << 30)
        })
        .unwrap_or(2)
}

fn meminfo_kb(key: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with(key))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn disk_free_gb(root: &Path) -> u64 {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_DISK_FREE_GB") {
        return v;
    }
    // -P forces POSIX single-line output; without it a long device name
    // wraps and the field offsets silently shift.
    run("df", &["-Pk", &root.to_string_lossy()])
        .and_then(|text| {
            let line = text.lines().nth(1)?.to_string();
            let avail_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
            Some(avail_kb / 1024 / 1024)
        })
        .unwrap_or(0)
}

fn on_battery() -> bool {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_ON_BATTERY") {
        return v != 0;
    }
    run("pmset", &["-g", "batt"])
        .map(|text| !text.contains("AC Power"))
        .unwrap_or(false)
}

/// Is something already using this box? A 16 GiB machine cannot host a
/// benchmark match AND a workspace build; the match owns the box.
///
/// Match the WORKLOAD, never the watchers: a `tail -f` on a match log
/// mentions the work without doing any of it — and it outlives the run.
/// Counting watchers pins the loop to the light tier forever after a single
/// benchmark ends, a failure that is permanent, silent, and looks like
/// caution.
fn contention() -> bool {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_CONTENTION") {
        return v != 0;
    }
    let hits = run("pgrep", &["-fl", "arenabench|harbor|cargo|docker"])
        .map(|text| text.lines().filter(|l| is_real_work(l)).count())
        .unwrap_or(0);
    if hits > 0 {
        return true;
    }
    // A running container is real work even when no host process names it —
    // but EXISTENCE is not activity: a finished match leaves recorder
    // sidecars up for hours. Ask what the containers are DOING.
    run(
        "docker",
        &["stats", "--no-stream", "--format", "{{.CPUPerc}}"],
    )
    .map(|text| {
        text.lines()
            .filter_map(|l| l.trim().trim_end_matches('%').parse::<f64>().ok())
            .any(|cpu| cpu > 20.0)
    })
    .unwrap_or(false)
}

/// One `pgrep -fl` line names real work when a workload command+verb pair
/// appears in it, and it is not a watcher (`tail`, `grep`, …) or a `sh -c`
/// wrapper mentioning the work without doing it.
fn is_real_work(line: &str) -> bool {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let is_cmd = |token: &str, name: &str| token == name || token.ends_with(&format!("/{name}"));

    for watcher in [
        "tail", "grep", "less", "watch", "pgrep", "ps", "awk", "sed", "tee",
    ] {
        if tokens.iter().any(|t| is_cmd(t, watcher)) {
            return false;
        }
    }
    for (i, t) in tokens.iter().enumerate() {
        let is_shell = ["sh", "bash", "zsh"].iter().any(|s| is_cmd(t, s));
        if is_shell
            && tokens.get(i + 1).is_some_and(|next| {
                next.starts_with('-')
                    && next.ends_with('c')
                    && next[1..].chars().all(|c| c.is_ascii_lowercase())
            })
        {
            return false;
        }
    }

    let pairs: &[(&str, &[&str])] = &[
        ("arenabench", &["run", "serve"]),
        ("harbor", &["run"]),
        ("cargo", &["build", "test", "clippy", "zigbuild", "install"]),
        ("docker", &["build", "compose"]),
    ];
    tokens.iter().enumerate().any(|(i, t)| {
        pairs.iter().any(|(cmd, verbs)| {
            is_cmd(t, cmd) && tokens.get(i + 1).is_some_and(|next| verbs.contains(next))
        })
    })
}

/// Read the whole supply picture for the governor.
pub(crate) fn supply(repo_root: &Path) -> Supply {
    let cpu = cpu_total();
    Supply {
        cpu,
        // An unmeasured load stands at half the core count: the one value that
        // is neither an escalation nor a shed. `Tier::Heavy` requires
        // `load1 < cpu / 2` and `Tier::Light` requires `load1 >= cpu`, so
        // `cpu / 2` satisfies neither and the governor runs its ordinary
        // cycle — which is what "we do not know" should buy.
        //
        // `0` bought the opposite: the heaviest tier on a box whose load
        // nobody read, which is the shape this module's own header rules out
        // ("degrades to a conservative value").
        load1: load1().unwrap_or(cpu / 2),
        mem_total_gb: mem_total_gb(),
        mem_free_gb: mem_free_gb(),
        disk_free_gb: disk_free_gb(repo_root),
        on_battery: on_battery(),
        busy: contention(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** A load the box will not report must not buy the
    /// heaviest tier.
    ///
    /// `load1` returned `0` when `/proc/loadavg` was absent and `uptime`
    /// could not be parsed — a container without `uptime`, or a BusyBox one
    /// spelling its output differently. Zero is an idle machine: it cannot
    /// trip `load1 >= cpu` (shed to Light) and it satisfies `load1 < cpu / 2`
    /// (escalate to Heavy — a full workspace build and a head-to-head bench).
    /// So the least information produced the most expensive decision, on a box
    /// that may have been saturated, and the tier's own reason string said
    /// "and idle" about a number nobody measured.
    #[test]
    fn an_unreadable_load_buys_neither_the_heavy_tier_nor_a_shed() {
        // The fallback the supply assembly uses, spelled the same way.
        let cpu = 16u32;
        let unmeasured = cpu / 2;

        assert!(
            !(unmeasured < cpu / 2),
            "an unmeasured load must not satisfy Heavy's idle test"
        );
        assert!(
            unmeasured < cpu,
            "nor trip the shed-to-Light branch, which would stop a healthy box"
        );
    }

    /// The parse says nothing rather than "idle" when the text is not a load.
    #[test]
    fn a_malformed_load_figure_reports_nothing_rather_than_rest() {
        assert_eq!(parse_leading_int("2.05"), Some(2));
        assert_eq!(parse_leading_int("0.00"), Some(0), "a real zero survives");
        assert_eq!(parse_leading_int(""), None);
        assert_eq!(parse_leading_int("n/a"), None);
        assert_eq!(parse_leading_int("load"), None);
    }

    /// The filter that keeps a finished benchmark's watchers from pinning the
    /// loop to the light tier forever — each shape below was a live
    /// misclassification in the shell driver's history.
    #[test]
    fn watchers_and_wrappers_are_not_work_but_the_workload_is() {
        assert!(is_real_work("812 cargo build -p stella-cli"));
        assert!(is_real_work("813 /usr/bin/arenabench run match.toml"));
        assert!(is_real_work("814 docker compose up"));
        assert!(!is_real_work("815 tail -f arenabench.log"));
        assert!(!is_real_work("816 grep cargo build history.txt"));
        assert!(!is_real_work("817 sh -c cargo build && echo done"));
        assert!(!is_real_work("818 /bin/zsh -lc cargo test"));
        assert!(!is_real_work("819 arenabench --version"));
        assert!(!is_real_work("820 docker ps"));
    }
}
