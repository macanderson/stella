// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Witness tests for #1587: what the installer writes, what the uninstaller
//! removes, and the two definition grammars byte-for-byte.
//!
//! Everything here drives the pure planning half over injected paths — no
//! service manager, no home directory, no environment. The `launchctl` /
//! `systemctl` calls themselves are environment-dependent and are exercised
//! manually (see the PR for the transcript); these tests pin everything those
//! calls are handed.

use std::path::Path;

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn spec<'a>(argv: &'a [String], keep_alive: bool) -> ServiceSpec<'a> {
    ServiceSpec {
        label: "monitor",
        exe: Path::new("/opt/homebrew/bin/stella"),
        args: argv,
        workdir: Path::new("/Users/dev/proj"),
        log_path: Path::new("/Users/dev/.stella/services/monitor.log"),
        keep_alive,
    }
}

#[test]
fn the_default_definition_starts_at_load_and_never_restarts() {
    let argv = args(&["monitor", "--interval", "300"]);
    let plist = launchd_plist(&spec(&argv, false)).unwrap();
    assert!(plist.contains("<key>RunAtLoad</key><true/>"), "{plist}");
    assert!(
        !plist.contains("KeepAlive"),
        "restart-on-exit must be opt-in — an agent that fails instantly under \
         KeepAlive is a spend loop: {plist}"
    );
    let unit = systemd_unit(&spec(&argv, false)).unwrap();
    assert!(unit.contains("WantedBy=default.target"), "{unit}");
    assert!(
        !unit.contains("Restart="),
        "same spend-loop rule on systemd: {unit}"
    );
    assert!(
        !unit.contains("StartLimit"),
        "a start limit without a restart policy is dead configuration: {unit}"
    );
}

#[test]
fn keep_alive_is_opt_in_throttled_and_start_limited() {
    let argv = args(&["monitor"]);
    let plist = launchd_plist(&spec(&argv, true)).unwrap();
    assert!(plist.contains("<key>KeepAlive</key><true/>"), "{plist}");
    assert!(
        plist.contains("<key>ThrottleInterval</key><integer>60</integer>"),
        "launchd's default 10s throttle restarts a failing agent six times a minute: {plist}"
    );
    let unit = systemd_unit(&spec(&argv, true)).unwrap();
    assert!(unit.contains("Restart=always\nRestartSec=60"), "{unit}");
    assert!(
        unit.contains("StartLimitIntervalSec=600") && unit.contains("StartLimitBurst=5"),
        "systemd has the brake launchd lacks — use it: {unit}"
    );
}

#[test]
fn the_shim_resolves_the_binary_at_load_and_fails_loudly() {
    let script = resolver_script("/opt/homebrew/bin/stella", "monitor");
    assert!(
        script.contains("bin=\"/opt/homebrew/bin/stella\""),
        "the recorded path is the first candidate: {script}"
    );
    assert!(
        script.contains("command -v stella"),
        "a moved binary (brew upgrade) falls back to PATH: {script}"
    );
    assert!(
        script.contains("exit 127") && script.contains(">&2"),
        "a missing binary must say so in the service log, not vanish: {script}"
    );
    assert!(
        script.contains("exec \"$bin\" \"$@\""),
        "the registered argv travels as positional parameters, never spliced \
         into the script: {script}"
    );
    assert!(!script.contains('\n'), "a systemd ExecStart= is one line");
}

#[test]
fn the_plist_escapes_xml_and_carries_each_argument_verbatim() {
    let argv = args(&["run", "fix <the> & queue"]);
    let plist = launchd_plist(&spec(&argv, false)).unwrap();
    assert!(
        plist.contains("<string>fix &lt;the&gt; &amp; queue</string>"),
        "{plist}"
    );
    assert!(plist.contains("<string>run</string>"), "{plist}");
    assert!(
        plist.contains("<key>Label</key><string>sh.oxagen.stella.monitor</string>"),
        "{plist}"
    );
    assert!(
        plist.contains("<key>WorkingDirectory</key><string>/Users/dev/proj</string>"),
        "the service runs where install was typed: {plist}"
    );
    assert!(
        plist.contains(
            "<key>StandardOutPath</key><string>/Users/dev/.stella/services/monitor.log</string>"
        ) && plist.contains(
            "<key>StandardErrorPath</key><string>/Users/dev/.stella/services/monitor.log</string>"
        ),
        "both streams land in the one documented console: {plist}"
    );
}

#[test]
fn the_exec_line_escapes_systemds_expansion_grammar() {
    let argv = args(&["run", "reach 100% of $HOME \"fast\""]);
    let unit = systemd_unit(&spec(&argv, false)).unwrap();
    assert!(
        unit.contains(r#""reach 100%% of $$HOME \"fast\"""#),
        "`%` and `$` expand at line level even inside quotes: {unit}"
    );
    assert!(
        unit.contains("$$PATH") && unit.contains("$$bin"),
        "the shim's own variables must reach sh, not systemd: {unit}"
    );
    assert!(
        unit.contains("' stella-monitor "),
        "argv rides after the single-quoted shim as separate words: {unit}"
    );
}

#[test]
fn paths_with_shell_metacharacters_are_refused_at_install_time() {
    let argv = args(&["monitor"]);
    let mut bad = spec(&argv, false);
    bad.workdir = Path::new("/Users/o'brien/proj");
    let err = launchd_plist(&bad).unwrap_err();
    assert!(
        err.contains("metacharacters"),
        "refusing loudly at install beats a definition that misfires at boot: {err}"
    );
    assert!(systemd_unit(&bad).is_err());
    let mut bad_exe = spec(&argv, false);
    bad_exe.exe = Path::new("/tmp/$evil/stella");
    assert!(launchd_plist(&bad_exe).is_err());
}

#[test]
fn a_control_character_cannot_reach_the_unit_file() {
    let argv = args(&["run", "two\nlines"]);
    let err = systemd_unit(&spec(&argv, false)).unwrap_err();
    assert!(
        err.contains("control characters"),
        "a newline in an ExecStart word would end the directive and inject \
         new ones: {err}"
    );
}

#[test]
fn labels_are_the_intersection_of_three_grammars() {
    validate_label("monitor-2").unwrap();
    for bad in ["", "Nope", "a b", "-x", "a/b", "über"] {
        assert!(validate_label(bad).is_err(), "{bad:?} must be rejected");
    }
    assert!(validate_label(&"x".repeat(LABEL_MAX + 1)).is_err());
    assert_eq!(
        ServiceManager::Launchd.file_name("monitor"),
        "sh.oxagen.stella.monitor.plist"
    );
    assert_eq!(
        ServiceManager::SystemdUser.file_name("monitor"),
        "stella-monitor.service"
    );
}

#[test]
fn a_label_derives_from_the_commands_first_word() {
    assert_eq!(derive_label(&args(&["monitor", "--x"])).unwrap(), "monitor");
    assert_eq!(derive_label(&args(&["Run!"])).unwrap(), "run");
    assert!(
        derive_label(&[]).is_err(),
        "nothing to register is an error, not an empty service"
    );
    assert!(derive_label(&args(&["---"])).is_err());
}

/// The #1587 witness: `install` writes exactly the planned definition where
/// the manager reads, refuses to replace it, and `uninstall`'s removal takes
/// it away again — with "already gone" reported as the distinct outcome it
/// is.
#[test]
fn install_writes_once_and_uninstall_removes() {
    let dir = tempfile::tempdir().unwrap();
    let argv = args(&["monitor"]);
    let plan = plan(ServiceManager::Launchd, &spec(&argv, false), dir.path()).unwrap();
    assert_eq!(
        plan.path,
        dir.path().join("sh.oxagen.stella.monitor.plist"),
        "the definition lands under the directory the manager reads"
    );

    write_definition(&plan).unwrap();
    let written = std::fs::read_to_string(&plan.path).unwrap();
    assert_eq!(written, plan.content, "written byte-for-byte as planned");

    let err = write_definition(&plan).unwrap_err();
    assert!(
        err.contains("uninstall"),
        "replacing a live definition silently would re-point a service the \
         operator installed on purpose: {err}"
    );

    assert_eq!(
        remove_definition(&plan.path).unwrap(),
        RemoveOutcome::Removed
    );
    assert!(!plan.path.exists());
    assert_eq!(
        remove_definition(&plan.path).unwrap(),
        RemoveOutcome::NothingInstalled,
        "uninstalling twice is the requested state, not a failure"
    );
}

/// #1627's install side: `--resume-all` registers the sweep and nothing else,
/// and refuses the two combinations that would turn a boot-time *resume* back
/// into a boot-time *restart* or a spend loop.
#[test]
fn resume_all_registers_the_sweep_and_refuses_a_command_or_keep_alive() {
    assert_eq!(
        resolve_command(true, false, &[]).unwrap(),
        crate::daemon::boot::registered_argv(),
        "--resume-all registers `stella daemon resume-all`, which continues \
         interrupted turns rather than starting anything over"
    );

    let err = resolve_command(true, false, &args(&["run", "ship it"])).unwrap_err();
    assert!(
        err.contains("--resume-all"),
        "a command beside --resume-all would leave `run` restarting work at \
         every boot: {err}"
    );

    let err = resolve_command(true, true, &[]).unwrap_err();
    assert!(
        err.contains("attempt bound"),
        "a restarted sweep would exhaust its own boot-attempt bound: {err}"
    );

    let err = resolve_command(false, false, &[]).unwrap_err();
    assert!(
        err.contains("--resume-all"),
        "the empty-command error must name the other way to ask: {err}"
    );
    assert_eq!(
        resolve_command(false, false, &args(&["monitor"])).unwrap(),
        args(&["monitor"]),
        "an ordinary registration is untouched"
    );
}

/// The registered boot definition carries the resume verb and no prompt — the
/// bytes launchd and systemd actually read at boot.
#[test]
fn the_boot_definition_registers_the_resume_sweep_on_both_platforms() {
    let argv = crate::daemon::boot::registered_argv();
    let mut spec = spec(&argv, false);
    spec.label = crate::daemon::boot::BOOT_LABEL;

    let plist = launchd_plist(&spec).unwrap();
    assert!(plist.contains("<string>daemon</string>"), "{plist}");
    assert!(plist.contains("<string>resume-all</string>"), "{plist}");
    assert!(
        !plist.contains("KeepAlive"),
        "the sweep resumes what a boot interrupted and exits: {plist}"
    );

    let unit = systemd_unit(&spec).unwrap();
    assert!(unit.contains("\"daemon\" \"resume-all\""), "{unit}");
    assert!(!unit.contains("Restart="), "{unit}");
}
