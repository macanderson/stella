// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! When the process wearing a pid started (#4511).
//!
//! A pid alone cannot answer "is the process that wrote this record still
//! there". Pids are recycled, and a recycled one makes a dead owner read as
//! live — so [`record::orphans`](super::record::orphans) skipped its record and
//! the checkout leaked with nothing naming it. The safe direction, and the
//! documented one, but a miss.
//!
//! A start time closes it, because the pair `(pid, started-at)` is unique for
//! as long as anything can observe either half: the kernel will hand the number
//! out again, but not with the same start instant.
//!
//! # What the number is, and what it is not
//!
//! It is **an opaque token**, comparable only against another reading of the
//! same pid from the same host and boot. Linux counts clock ticks since boot;
//! macOS counts microseconds since the epoch. Nothing here converts between
//! them or renders one, and a caller that treated it as a timestamp would be
//! reading a different clock on each platform.
//!
//! # Where there is no reading
//!
//! [`of`] answers `None` — on a platform with neither interface, and on any
//! failure of the two that exist (a `/proc` this container did not mount, a
//! process that exited between the probe and the read, a `sysctl` that refused).
//! `None` is not "the owner is gone": it is "this host cannot tell", and the
//! sweep must keep the pre-existing safe direction there rather than invent an
//! answer. See [`record`](super::record)'s own doc for the resulting rule.

#[cfg(target_os = "macos")]
use std::ffi::c_int;

/// The start token of the process wearing `pid`, if this host can read one.
#[cfg(target_os = "linux")]
pub(super) fn of(pid: u32) -> Option<u64> {
    parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Pull `starttime` (field 22) out of one `/proc/<pid>/stat` line.
///
/// Split at the **last** `)` rather than by whitespace from the left: field 2
/// is the executable name in parentheses, it is attacker-chosen, and it may
/// contain both spaces and parentheses. `comm` of `a b) c` is a legal
/// executable name and defeats every simpler split.
#[cfg(target_os = "linux")]
fn parse_proc_stat(line: &str) -> Option<u64> {
    let after_comm = &line[line.rfind(')')? + 1..];
    // `state` is the first field after the name, so `starttime` — field 22 of
    // the line — is index 19 counting from there.
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// The start token of the process wearing `pid`, if this host can read one.
///
/// `PROC_PIDTBSDINFO` fills one [`libc::proc_bsdinfo`], whose
/// `pbi_start_tv{sec,usec}` pair is that process's own start instant. Folded to
/// microseconds so the token is one integer to compare, never a time to render.
///
/// `libproc` rather than `sysctl(KERN_PROC_PID)`: `libc` exposes this struct on
/// Apple targets and does not expose `kinfo_proc`, so the `sysctl` route would
/// need a hand-declared C layout — a second copy of a kernel structure to keep
/// correct, for a number two documented fields already carry.
#[cfg(target_os = "macos")]
pub(super) fn of(pid: u32) -> Option<u64> {
    // SAFETY: `proc_bsdinfo` is a plain C aggregate of integers and `c_char`
    // arrays, for which all-zero is a valid value. `proc_pidinfo` overwrites it
    // on success, and the size check below is what stops the zeroes being read
    // when it did not.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is a live `proc_bsdinfo` this frame owns and `size` is
    // exactly its size, which is the buffer contract `proc_pidinfo(3)`
    // documents for `PROC_PIDTBSDINFO`. It writes only into that buffer and
    // never more than `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            c_int::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    // It returns the byte count it filled, and answers 0 for a pid nothing is
    // wearing — which would otherwise read as a start time of zero shared by
    // every dead process. A short write is a struct this build did not expect.
    if written != size {
        return None;
    }
    info.pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)
}

/// No reading on this platform — see the module doc for why that is a distinct
/// answer from "the owner is gone".
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn of(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This process is alive, so whatever this host can read for it must be
    /// readable — and stable across two reads, or it is not an identity.
    #[test]
    fn a_live_process_reads_the_same_token_twice() {
        let me = std::process::id();
        assert_eq!(of(me), of(me));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(
            of(me).is_some(),
            "this host has an interface; a process reading its own start time must find it"
        );
    }

    /// A pid nothing is wearing has no reading. That is what lets the sweep
    /// tell a recycled pid from the process that wrote the record.
    #[test]
    fn a_pid_no_process_wears_has_no_token() {
        // 0 is the kernel's own scheduler slot on both interfaces: `/proc/0`
        // does not exist and `KERN_PROC_PID` resolves nothing for it, so it is
        // the one number guaranteed not to name a process this test could race.
        assert_eq!(of(0), None);
    }

    /// `comm` is attacker-chosen and may hold spaces and parentheses, so the
    /// parse splits at the last `)` and counts from there.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_hostile_executable_name_does_not_move_the_field() {
        // `state` is index 0, so eighteen fillers put `starttime` at index 19.
        let fillers: String = (1..=18).map(|n| format!("{n} ")).collect();
        let line = format!("42 (evil ) name) S {fillers}9876543 rest here");
        assert_eq!(parse_proc_stat(&line), Some(9_876_543));
    }
}
