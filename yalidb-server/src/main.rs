//! Bounded loopback HTTP adapter. The overall database remains experimental.
#![forbid(unsafe_code)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::ExitCode;
mod transport;
use yalidb_core::{Condition, Db, DeleteCondition, Entry, Error};

const MAX_BODY: usize = 96 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    key: String,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireCondition {
    Any,
    Absent,
    Version { version: String },
}
impl WireCondition {
    fn condition(self) -> Result<Condition, Error> {
        Ok(match self {
            Self::Any => Condition::Any,
            Self::Absent => Condition::Absent,
            Self::Version { version } => Condition::Version(parse_version(&version)?),
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Put {
    key: String,
    value: String,
    ttl_ms: Option<u64>,
    condition: Option<WireCondition>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delete {
    key: String,
    condition: Option<WireCondition>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scan {
    prefix: String,
    after_key: Option<String>,
    limit: Option<usize>,
}

fn parse_version(s: &str) -> Result<u64, Error> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return Err(Error::Invalid("version must be a canonical decimal string"));
    }
    s.parse()
        .map_err(|_| Error::Invalid("version out of range"))
}
fn decode(s: &str) -> Result<Vec<u8>, Error> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::Invalid("expected base64url without padding"))
}
fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|_| Error::Invalid("malformed JSON or unknown fields"))
}
fn entry(e: Entry) -> Value {
    json!({"value":URL_SAFE_NO_PAD.encode(e.value),"version":e.version.to_string(),"expires_at_ms":e.expires_at_ms.map(|v|v.to_string())})
}
fn dispatch(db: &Db, path: &str, body: &[u8]) -> Result<Value, Error> {
    match path {
        "/v1/get" => {
            let r: Key = parse(body)?;
            Ok(match db.get(&decode(&r.key)?)? {
                Some(e) => json!({"found":true,"entry":entry(e)}),
                None => json!({"found":false}),
            })
        }
        "/v1/put" => {
            let r: Put = parse(body)?;
            let receipt = db.put(
                &decode(&r.key)?,
                &decode(&r.value)?,
                r.ttl_ms,
                r.condition
                    .map(WireCondition::condition)
                    .transpose()?
                    .unwrap_or(Condition::Any),
            )?;
            Ok(
                json!({"version":receipt.version.to_string(),"expires_at_ms":receipt.expires_at_ms.map(|v|v.to_string())}),
            )
        }
        "/v1/delete" => {
            let r: Delete = parse(body)?;
            let condition = match r
                .condition
                .map(WireCondition::condition)
                .transpose()?
                .unwrap_or(Condition::Any)
            {
                Condition::Any => DeleteCondition::Any,
                Condition::Version(v) => DeleteCondition::Version(v),
                Condition::Absent => {
                    return Err(Error::Invalid("delete does not support absent condition"));
                }
            };
            Ok(json!({"deleted":db.delete(&decode(&r.key)?,condition)?}))
        }
        "/v1/scan" => {
            let r: Scan = parse(body)?;
            let after = r.after_key.as_deref().map(decode).transpose()?;
            let page = db.scan_prefix(
                &decode(&r.prefix)?,
                after.as_deref(),
                r.limit.unwrap_or(100),
            )?;
            Ok(
                json!({"entries":page.entries.into_iter().map(|(k,v)|json!({"key":URL_SAFE_NO_PAD.encode(k),"entry":entry(v)})).collect::<Vec<_>>(),"next_key":page.next_key.map(|k|URL_SAFE_NO_PAD.encode(k))}),
            )
        }
        _ => Err(Error::Invalid("unknown operation")),
    }
}
fn failure(error: Error) -> (u16, Value) {
    let (status, code) = match error {
        Error::Invalid(_) | Error::Clock => (400, "invalid_input"),
        Error::Conflict => (409, "conflict"),
        Error::Capacity(_) => (507, "capacity"),
        _ => (503, "storage_unavailable"),
    };
    // Storage paths and error internals stay off the wire. Mutation errors may be ambiguous.
    (
        status,
        json!({"error":code,"mutation_outcome_may_be_unknown":status==503}),
    )
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |different, (a, b)| different | (a ^ b))
        == 0
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match transport::serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_contract_and_conditional_updates() {
        let db = Db::memory();
        assert_eq!(
            dispatch(&db, "/v1/get", br#"{"key":"aw"}"#).unwrap(),
            json!({"found":false})
        );
        let r = dispatch(
            &db,
            "/v1/put",
            br#"{"key":"aw","value":"dg","condition":{"kind":"absent"}}"#,
        )
        .unwrap();
        assert_eq!(r["version"], "1");
        assert!(matches!(
            dispatch(
                &db,
                "/v1/put",
                br#"{"key":"aw","value":"eA","condition":{"kind":"version","version":"99"}}"#
            ),
            Err(Error::Conflict)
        ));
        assert_eq!(
            dispatch(&db, "/v1/get", br#"{"key":"aw"}"#).unwrap()["entry"]["value"],
            "dg"
        );
        assert!(
            dispatch(
                &db,
                "/v1/put",
                br#"{"key":"aw","value":"dg","unexpected":1}"#
            )
            .is_err()
        );
        assert!(dispatch(&db, "/v1/get", br#"{"key":"%%%"}"#).is_err());
        assert!(parse_version("01").is_err());
        assert!(parse_version("18446744073709551616").is_err());
    }
}
