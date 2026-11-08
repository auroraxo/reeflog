//! Committed measurement artifacts must not carry the author's host paths.
//!
//! Why this test exists (2026-11-08): several committed `research/` artifacts
//! from earlier sessions recorded the machine they were produced on —
//! `/home/...` binary paths, `/tmp/...` journal locations. The measurement
//! pin tests added earlier in this family each assert "no host path" for
//! THEIR artifact, but nothing asserted it for the whole directory, so the
//! directory drifted: a scan found ten committed files carrying host paths.
//!
//! The guard is deliberately dumb: every file under `research/` is read as
//! bytes and checked for the four path markers that leak a machine
//! (`/home/`, `/tmp/`, `/Users/`, `C:\`). No regex over JSON, no allowlist —
//! an allowlist is how the first ten files got through.
//!
//! Falsified by construction: put any `/tmp/` string back into any artifact
//! and this test goes red naming the file; delete the loop body and it goes
//! green while the paths remain.

use std::fs;
use std::path::PathBuf;

const MARKERS: [&str; 4] = ["/home/", "/tmp/", "/Users/", "C:\\"];

#[test]
fn no_committed_artifact_carries_a_host_path() {
    let research = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("research");
    let mut hits = Vec::new();
    let mut scanned = 0usize;

    for entry in fs::read_dir(&research).expect("research/ is readable") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .to_string();
        scanned += 1;
        let bytes = fs::read(&path).expect("file readable");
        let text = String::from_utf8_lossy(&bytes);
        for (lineno, line) in text.lines().enumerate() {
            for marker in MARKERS {
                if line.contains(marker) {
                    hits.push(format!(
                        "{}:{} contains {}",
                        name,
                        lineno + 1,
                        marker.escape_default()
                    ));
                }
            }
        }
    }

    assert!(
        scanned > 0,
        "research/ contained no files — the guard would pass vacuously"
    );
    assert!(
        hits.is_empty(),
        "committed artifacts carry host paths ({} file(s)):\n{}",
        hits.len(),
        hits.join("\n")
    );
}
