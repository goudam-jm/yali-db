//! Process death tests establish acknowledged-prefix recovery, not power-loss safety.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};
use yalidb_core::{Condition, Db};

#[test]
#[ignore = "child-process test entry point, invoked by kill_after_acknowledgments"]
fn crash_worker() {
    let path = std::env::var("YALIDB_CRASH_TEST_DIRECTORY").unwrap();
    let db = Db::open(path).unwrap();
    for n in 1..100_000u64 {
        db.put(b"counter", &n.to_le_bytes(), None, Condition::Any)
            .unwrap();
        println!("ACK {n}");
        std::io::stdout().flush().unwrap();
        if n % 3 == 0 {
            db.compact().unwrap();
        }
    }
}

#[test]
fn kill_after_acknowledgments() {
    for acknowledged in [1, 3, 8, 15] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_worker", "--ignored", "--nocapture"])
            .env("YALIDB_CRASH_TEST_DIRECTORY", &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut seen = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if line.trim() == format!("ACK {acknowledged}") {
                seen = true;
                break;
            }
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(seen, "worker exited before acknowledging writes");
        let db = Db::open(&path).unwrap();
        let count = u64::from_le_bytes(
            db.get(b"counter")
                .unwrap()
                .unwrap()
                .value
                .try_into()
                .unwrap(),
        );
        assert!(count >= acknowledged);
        let version = db
            .put(b"after", b"restart", None, Condition::Any)
            .unwrap()
            .version;
        assert!(version > acknowledged);
        drop(db);
        assert!(fs::metadata(path.join("MANIFEST")).unwrap().is_file());
    }
}
