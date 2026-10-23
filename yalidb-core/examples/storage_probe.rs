//! Synthetic workload driver for disposable Linux validation; not an administration tool.
use std::io::{self, BufRead, Write};
use yalidb_core::{Condition, Db, DeleteCondition, Error};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let command = args
        .get(1)
        .ok_or("storage_probe COMMAND DIRECTORY [SEED]")?;
    let path = args.get(2).ok_or("directory required")?;
    if command == "locked" {
        assert!(matches!(Db::open(path), Err(Error::Locked)));
        return Ok(());
    }
    let db = Db::open(path)?;
    match command.as_str() {
        "worker" => {
            let mut seed: u64 = args.get(3).ok_or("seed required")?.parse()?;
            let mut input = io::stdin().lock();
            let mut line = String::new();
            let mut step = 0;
            loop {
                line.clear();
                if input.read_line(&mut line)? == 0 {
                    break;
                }
                if line.trim() != "GO" {
                    return Err("expected GO".into());
                }
                step += 1;
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let key = format!("slot:{}", seed % 16);
                let delete = seed.is_multiple_of(5);
                println!(
                    "BEGIN {step} {} {key} {seed}",
                    if delete { "D" } else { "P" }
                );
                io::stdout().flush()?;
                if step % 7 == 0 {
                    db.compact()?;
                }
                if delete {
                    db.delete(key.as_bytes(), DeleteCondition::Any)?;
                } else {
                    db.put(key.as_bytes(), &seed.to_le_bytes(), None, Condition::Any)?;
                }
                if step % 11 == 0 {
                    db.compact()?;
                }
                println!(
                    "ACK {step} {} {} {key} {seed}",
                    db.stats()?.sequence,
                    if delete { "D" } else { "P" }
                );
                io::stdout().flush()?;
            }
        }
        "dump" => {
            println!("META {}", db.stats()?.sequence);
            for (key, entry) in db.scan_prefix(b"", None, 100)?.entries {
                let value = u64::from_le_bytes(
                    entry
                        .value
                        .try_into()
                        .map_err(|_| "expected 8-byte synthetic value")?,
                );
                println!(
                    "VALUE {} {value} {}",
                    String::from_utf8(key)?,
                    entry.version
                );
            }
        }
        "hold" => {
            db.put(b"held", b"value", None, Condition::Any)?;
            println!("HELD");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
        }
        "init-enospc" => {
            db.put(b"anchor", &[42; 1024], None, Condition::Any)?;
        }
        "expect-enospc" => {
            for n in 0..16u64 {
                let key = format!("doomed:{n}");
                match db.put(key.as_bytes(), &[165; 65536], None, Condition::Any) {
                    Ok(receipt) => {
                        println!("CONFIRMED {n} {}", receipt.version);
                        io::stdout().flush()?;
                    }
                    Err(Error::Io(error)) => {
                        assert_eq!(error.raw_os_error(), Some(28));
                        assert!(!db.stats()?.healthy);
                        assert!(matches!(db.get(b"anchor"), Err(Error::Unhealthy)));
                        println!("ENOSPC_OWNER_POISONED {n}");
                        return Ok(());
                    }
                    Err(_) => panic!("expected real I/O ENOSPC, not a logical admission limit"),
                }
            }
            panic!("filesystem must actually be full after exhausting speculative preallocation");
        }
        "verify-enospc" => {
            let confirmed: u64 = args.get(3).ok_or("confirmed put count required")?.parse()?;
            let anchor = db.get(b"anchor")?.ok_or("acknowledged anchor missing")?;
            assert_eq!(anchor.value, [42; 1024]);
            assert_eq!(anchor.version, 1);
            let mut recovered = 0;
            for n in 0..=confirmed {
                if let Some(entry) = db.get(format!("doomed:{n}").as_bytes())? {
                    assert_eq!(entry.value, vec![165; 65536]);
                    assert_eq!(entry.version, n + 2);
                    recovered += 1;
                } else {
                    assert_eq!(n, confirmed, "acknowledged put missing");
                }
            }
            assert!((confirmed..=confirmed + 1).contains(&recovered));
            assert_eq!(db.stats()?.sequence, 1 + recovered);
            println!("ACKNOWLEDGED_ANCHOR_PRESERVED");
        }
        _ => return Err("unknown probe command".into()),
    }
    Ok(())
}
