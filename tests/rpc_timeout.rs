//! The RPC call must not be able to hang forever.
//!
//! Measured 2026-11-02 (live mainnet-beta, `reeflog start`): a second
//! `start` on an existing journal hung indefinitely — 4 of 5 runs on the
//! then-current tree never returned (killed at 45 s, exit 124) while the
//! same sequence completed in seconds on healthy runs. Mechanism read from
//! the code and confirmed by the ureq 3.5 docs ("By default no timeouts are
//! set"): `HttpFetcher::call` posts to the RPC endpoint with no request
//! timeout, and the daemon's shutdown path joins the payload worker
//! unbounded — one stalled HTTP peer parks the whole run past its own
//! `--seconds` deadline. The webhook poster already carries a global
//! timeout for exactly this reason.
//!
//! These tests drive the REAL binary against a local TCP peer that accepts
//! the request and never answers — the exact shape of a stalled public RPC
//! endpoint. `verify --anchor-slot` is the smallest guaranteed-RPC path:
//! resolving the anchor comes from the chain, so it calls `getBlock` even
//! against a completely empty journal (where the walk itself would return
//! early without touching the network).
//!
//! The runner KILLS the child at a hard deadline instead of waiting
//! forever, so a regression fails the build instead of hanging CI.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
/// Hard bound for one command against a stalled peer: the fixed fetcher
/// retries a 10 s timeout at most 6 times (~60 s + ~20 s of capped
/// backoff), so a healthy run finishes well under this. The pre-fix
/// behavior was "never", so anything finite with margin works as the gate.
/// Hard bound for one command against a stalled peer: the fixed fetcher
/// gives a stalled endpoint two attempts of 10 s plus one 400 ms backoff
/// per call, and every shutdown pass stops at its first failed call — so a
/// healthy run finishes well under this. The pre-fix behavior was "never"
/// (ureq sets no request timeout by default), so any finite bound with
/// margin works as the gate.
const KILL_DEADLINE: Duration = Duration::from_secs(110);

/// A TCP server that accepts connections, reads the request, and holds the
/// connection open WITHOUT ever answering.
fn stalling_server() -> (String, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop2.load(Ordering::Relaxed) {
                break;
            }
            let mut stream: TcpStream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            std::thread::spawn(move || {
                // Read the request so the client's send completes, then
                // never answer. The client must time out on its own.
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                    if stream.write_all(b"").is_err() {
                        break;
                    }
                }
            });
        }
    });

    (format!("http://{addr}"), stop)
}

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("reeflog binary runs")
}

/// Run the binary with a hard deadline: if it has not returned when the
/// deadline passes, kill it and report the hang as a failure. A test that
/// hangs is worse than a test that fails — it hides the result instead of
/// demanding the fix.
fn run_bounded(dir: &Path, args: &[&str]) -> Result<Output, Duration> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("reeflog binary runs");

    let started = Instant::now();
    loop {
        match child.try_wait().expect("wait") {
            Some(_) => {
                let out = child
                    .wait_with_output()
                    .expect("collect output of finished child");
                return Ok(out);
            }
            None => {
                if started.elapsed() > KILL_DEADLINE {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(started.elapsed());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[test]
fn verify_against_a_stalled_rpc_endpoint_reports_failure_instead_of_hanging() {
    let (url, stop) = stalling_server();

    let dir = unique_dir("verify-stall");
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");

    let out = run_bounded(
        &dir,
        &[
            "verify",
            "--db",
            "reeflog.db",
            "--rpc",
            &url,
            "--anchor-slot",
            "400000000",
            "--anchor-tries",
            "1",
        ],
    );

    stop.store(true, Ordering::Relaxed);

    let out = match out {
        Ok(o) => o,
        Err(hung) => panic!(
            "verify had to be KILLED after {hung:?} against a stalled RPC endpoint — \
             HttpFetcher::call is not bounded and the daemon cannot be stopped \
             (measured 2026-11-02: 4 of 5 start runs hung this way pre-fix)"
        ),
    };

    assert!(
        !out.status.success(),
        "a stalled endpoint must not be reported as success (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        err.contains("timed out") || err.contains("anchor"),
        "the failure must NAME the mechanism (timeout) or the failed step (anchor), got: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_healthy_path_still_verifies() {
    // False-positive guard: the timeout must not turn a working endpoint
    // into a failure. This is the only check here that talks to the real
    // network; it must keep passing on both sides of the fix.
    let dir = unique_dir("verify-healthy");
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");

    // No events in the journal: the walk returns early (empty journal,
    // nothing claimed) — this exercises argument handling and the anchor
    // path against a LIVE endpoint without depending on window contents.
    let out = run_bounded(
        &dir,
        &[
            "verify",
            "--db",
            "reeflog.db",
            "--anchor-slot",
            "400000000",
            "--anchor-tries",
            "1",
        ],
    );

    let out = match out {
        Ok(o) => o,
        Err(hung) => panic!("verify hung for {hung:?} against the LIVE endpoint"),
    };

    // The verdict itself is exit 0 or 3 depending on the empty-journal
    // window; what must hold is that the call RETURNED and said so.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"complete\""),
        "verify must print its verdict (stdout: {stdout})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
