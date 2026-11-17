//! A detached child leads a process group of its own and has null standard streams.
//!
//! An integration test, not a unit test: it spawns, and a spawn in the middle of the
//! unit tests would let the child hold copies of other tests' locked files for a
//! moment (see `src/detach.rs`). Alone in its own process, nothing else is running.

#![cfg(target_os = "linux")]

#[path = "../src/detach.rs"]
mod detach;

use std::process::Command;

/// The process group id (field 5 of `/proc/<pid>/stat`).
fn group_of(stat: &str) -> u32 {
    // The command name is in parentheses and may hold spaces: count from the last `)`.
    let after = stat.rsplit_once(')').expect("a stat line").1;
    let group = after.split_whitespace().nth(2).expect("group field");
    group.parse().unwrap()
}

#[test]
fn a_detached_child_has_its_own_process_group_and_null_standard_streams() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report");
    // Reads where its own descriptors point and what group it is in, then writes
    // that to a file (its standard output is /dev/null, so it redirects only the
    // final write; a redirect around the reads would change what they find).
    let script = format!(
        "fd0=$(readlink /proc/$$/fd/0); fd1=$(readlink /proc/$$/fd/1); \
         fd2=$(readlink /proc/$$/fd/2); stat=$(cat /proc/$$/stat); \
         printf '%s\\n' \"$fd0\" \"$fd1\" \"$fd2\" \"$stat\" > {}",
        report.display()
    );
    let mut child = detach::detach(Command::new("sh").args(["-c", script.as_str()]))
        .spawn()
        .expect("spawn a stand-in");
    let pid = child.id();
    assert!(child.wait().unwrap().success());

    let text = std::fs::read_to_string(&report).unwrap();
    let mut lines = text.lines();
    for fd in 0..3 {
        assert_eq!(lines.next(), Some("/dev/null"), "descriptor {fd}");
    }
    let child_group = group_of(lines.next().expect("stat line"));
    let our_group = group_of(&std::fs::read_to_string("/proc/self/stat").unwrap());
    assert_eq!(child_group, pid, "it leads a process group of its own");
    assert_ne!(child_group, our_group);
}
