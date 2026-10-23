use yalidb_core::{Condition, Db};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run -p yalidb-core --example user_state -- DIRECTORY")?;
    let db = Db::open(directory)?;
    let prior = db.get(b"user:demo:theme")?;
    let condition = prior
        .as_ref()
        .map_or(Condition::Absent, |e| Condition::Version(e.version));
    let receipt = db.put(b"user:demo:theme", b"dark", None, condition)?;
    println!("stored version {}", receipt.version);
    println!("{} indexed keys", db.stats()?.indexed_keys);
    Ok(())
}
