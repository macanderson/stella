//! Resource probes for the self-driving governor — cross-platform, best-effort,
//! never fatal. Every probe returns a NUMBER and degrades to a conservative
//! value rather than failing: a governor that dies because `pmset` is missing
//! is worse than one that assumes mains power.
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

use stella_core::self_driving::Supply;

fn env_override(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn cpu_total() -> u32 {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_CPU") {
        return v as u32;
    }
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(2)
}

/// Integer part of the 1-minute load average; the governor compares
/// magnitudes, not decimals.
fn load1() -> u32 {
    if let Some(v) = env_override("SELF_DRIVING_PROBE_LOAD1") {
        return v as u32;
    }
    if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
        return parse_leading_int(text.split_whitespace().next().unwrap_or("0"));
    }
    // `uptime` tails with "load averages: 2.05 2.33 2.55" (macOS spells it
    // "averages", Linux "average:").
    run("uptime", &[])
        .and_then(|text| {
            let tail = text.rsplit("load average").next()?.to_string();
            let first = tail
                .trim_start_matches(|c: char| c == 's' || c == ':' || c.is_whitespace())
                .split([',', ' '])
                .next()?
                .to_string();
            Some(parse_leading_int(&first))
        })
        .unwrap_or(0)
}

fn parse_leading_int(text: &str) -> u32 {
    text.split('.')
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
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
    Supply {
        cpu: cpu_total(),
        load1: load1(),
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
