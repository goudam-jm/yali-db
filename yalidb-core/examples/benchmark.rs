//! Synthetic embedded measurements; never opens an existing path in load mode.
use std::{collections::HashMap, fs, io::Write, path::Path, time::Instant};
use yalidb_core::{Condition, Db};

fn resource(stage: &str) -> Result<(), Box<dyn std::error::Error>> {
    for (path, fields) in [
        (
            "/proc/self/status",
            vec![
                ("VmRSS:", "rss_bytes", 1024),
                ("VmHWM:", "peak_rss_bytes", 1024),
            ],
        ),
        (
            "/proc/self/io",
            vec![("write_bytes:", "process_write_bytes", 1)],
        ),
    ] {
        let text = fs::read_to_string(path)?;
        for (field, name, scale) in fields {
            let row = text
                .lines()
                .find(|row| row.starts_with(field))
                .ok_or("missing process accounting field")?;
            let value: u64 = row
                .split_whitespace()
                .nth(1)
                .ok_or("missing accounting value")?
                .parse()?;
            println!("STAT\t{stage}\t{name}\t{}", value * scale);
        }
    }
    Ok(())
}

fn key(index: usize) -> [u8; 8] {
    (index as u64).to_be_bytes()
}
fn value(index: usize, size: usize, updated: bool) -> Vec<u8> {
    vec![((index + usize::from(updated)) % 251) as u8; size]
}
fn next(seed: &mut u64, keys: usize) -> usize {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed % keys as u64) as usize
}
fn samples(phase: &str, values: &[u64], elapsed: u128) {
    for value in values {
        println!("SAMPLE\t{phase}\t{value}");
    }
    println!("ELAPSED\t{phase}\t{elapsed}\t{}", values.len());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 7 {
        return Err("benchmark load|recover DIRECTORY KEYS VALUE_BYTES OPERATIONS SEED".into());
    }
    let mode = &args[1];
    let path = Path::new(&args[2]);
    let keys: usize = args[3].parse()?;
    let size: usize = args[4].parse()?;
    let operations: usize = args[5].parse()?;
    let seed: u64 = args[6].parse()?;
    if !(1..=100_000).contains(&keys)
        || !(1..=65536).contains(&size)
        || !(1..=100_000).contains(&operations)
        || seed == 0
    {
        return Err("invalid benchmark dimensions".into());
    }
    if mode == "load" {
        if path.exists() {
            return Err("load requires a new database path".into());
        }
    } else if mode != "recover" || !path.is_dir() {
        return Err("recover requires an existing benchmark directory".into());
    }
    resource("runtime")?;
    let start = Instant::now();
    let db = Db::open(path)?;
    let sequence = db.stats()?.sequence;
    println!("DURATION\topen\t{}", start.elapsed().as_nanos());
    resource("opened")?;
    if mode == "load" {
        assert_eq!(sequence, 0);
        let mut times = Vec::with_capacity(keys);
        let phase = Instant::now();
        for index in 0..keys {
            let (key, value) = (key(index), value(index, size, false));
            let start = Instant::now();
            let receipt = db.put(&key, &value, None, Condition::Any)?;
            times.push(start.elapsed().as_nanos() as u64);
            assert_eq!(receipt.version, index as u64 + 1);
        }
        let elapsed = phase.elapsed().as_nanos();
        resource("populated")?;
        samples("populate", &times, elapsed);
        times.clear();
        let mut random = seed;
        let phase = Instant::now();
        for _ in 0..operations {
            let index = next(&mut random, keys);
            let key = key(index);
            let start = Instant::now();
            let entry = db.get(&key)?.ok_or("missing benchmark key")?;
            times.push(start.elapsed().as_nanos() as u64);
            assert_eq!(entry.value, value(index, size, false));
            assert_eq!(entry.version, index as u64 + 1);
        }
        samples("warm_get", &times, phase.elapsed().as_nanos());
        times.clear();
        random = seed;
        let phase = Instant::now();
        for operation in 0..operations {
            let index = next(&mut random, keys);
            let (key, value) = (key(index), value(index, size, true));
            let start = Instant::now();
            let receipt = db.put(&key, &value, None, Condition::Any)?;
            times.push(start.elapsed().as_nanos() as u64);
            assert_eq!(receipt.version, (keys + operation + 1) as u64);
        }
        let elapsed = phase.elapsed().as_nanos();
        resource("updated")?;
        samples("overwrite", &times, elapsed);
        std::io::stdout().flush()?;
        // Leave the selected log intact for a fresh-process recovery measurement.
        std::process::exit(0);
    }
    assert_eq!(sequence, (keys + operations) as u64);
    assert_eq!(db.stats()?.indexed_keys, keys);
    let start = Instant::now();
    db.compact()?;
    println!("DURATION\tmaintenance\t{}", start.elapsed().as_nanos());
    resource("maintained")?;
    let mut random = seed;
    let mut updated = HashMap::new();
    for operation in 0..operations {
        updated.insert(next(&mut random, keys), (keys + operation + 1) as u64);
    }
    for index in 0..keys {
        let entry = db.get(&key(index))?.ok_or("recovery lost a key")?;
        assert_eq!(
            entry.value,
            value(index, size, updated.contains_key(&index))
        );
        assert_eq!(
            entry.version,
            updated.get(&index).copied().unwrap_or(index as u64 + 1)
        );
    }
    println!("VERIFIED\t{keys}\t{sequence}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn workload_matches_python_unsigned_xorshift64() {
        let mut seed = 42;
        let actual: Vec<_> = (0..5).map(|_| super::next(&mut seed, 10_000)).collect();
        assert_eq!(actual, [5674, 5471, 954, 9736, 4162]);
    }
}
