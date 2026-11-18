//! JSON-RPC 2.0 envelopes and request parsing, independent of transport (ADR 0017).

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Version {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(Number),
    String(String),
    Null,
}

// Only a missing id makes a notification. An explicit null id still gets a reply.
fn present_id<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Id>, D::Error> {
    Id::deserialize(de).map(Some)
}

fn params<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Value>, D::Error> {
    match Value::deserialize(de)? {
        v @ (Value::Array(_) | Value::Object(_)) => Ok(Some(v)),
        _ => Err(de::Error::custom("params must be an object or array")),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: Version,
    #[serde(
        default,
        deserialize_with = "present_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<Id>,
    pub method: String,
    #[serde(
        default,
        deserialize_with = "params",
        skip_serializing_if = "Option::is_none"
    )]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Error {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "Parse error".into(),
            data: None,
        }
    }

    fn invalid_request() -> Self {
        Self {
            code: -32600,
            message: "Invalid Request".into(),
            data: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: Version,
    pub id: Id,
    #[serde(flatten)]
    pub payload: Payload,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Payload {
    Success { result: Value },
    Failure { error: Error },
}

#[derive(Debug, PartialEq)]
pub enum Incoming {
    Single(Request),
    /// A batch: Pinna's server refuses it whole, so its members aren't read.
    Batch,
}

fn request(value: Value) -> Result<Request, Error> {
    if !value.is_object() {
        return Err(Error::invalid_request());
    }
    serde_json::from_value(value).map_err(|_| Error::invalid_request())
}

pub fn parse(body: &[u8]) -> Result<Incoming, Error> {
    match serde_json::from_slice(body).map_err(|_| Error::parse_error())? {
        Value::Array(items) if items.is_empty() => Err(Error::invalid_request()),
        Value::Array(_) => Ok(Incoming::Batch),
        value => request(value).map(Incoming::Single),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_and_notifications_round_trip() {
        for (id, expected) in [
            (None, None),
            (Some(json!(null)), Some(Id::Null)),
            (Some(json!(0)), Some(Id::Number(0.into()))),
            (Some(json!("abc")), Some(Id::String("abc".into()))),
        ] {
            let mut wire = json!({"jsonrpc":"2.0", "method":"ping", "params":{}});
            if let Some(id) = id {
                wire["id"] = id;
            }
            let Incoming::Single(req) = parse(&serde_json::to_vec(&wire).unwrap()).unwrap() else {
                panic!("expected a single request")
            };
            assert_eq!(req.id, expected);
            assert_eq!(serde_json::to_value(req).unwrap(), wire);
        }
    }

    #[test]
    fn a_non_empty_array_is_a_batch() {
        let batch = br#"[{"jsonrpc":"2.0","method":"ping","id":1},false]"#;
        assert_eq!(parse(batch).unwrap(), Incoming::Batch);
    }

    #[test]
    fn syntax_and_request_errors_have_distinct_codes() {
        for body in ["", "{", "[] trailing"] {
            assert_eq!(parse(body.as_bytes()).unwrap_err().code, -32700);
        }
        for body in [
            "[]",
            "null",
            "{}",
            r#"{"jsonrpc":"1.0","method":"ping"}"#,
            r#"{"jsonrpc":"2.0","method":5}"#,
            r#"{"jsonrpc":"2.0","method":"ping","id":true}"#,
            r#"{"jsonrpc":"2.0","method":"ping","params":null}"#,
        ] {
            assert_eq!(parse(body.as_bytes()).unwrap_err().code, -32600);
        }
    }

    #[test]
    fn responses_have_either_a_result_or_an_error() {
        for wire in [
            json!({"jsonrpc":"2.0", "id":1, "result":null}),
            json!({"jsonrpc":"2.0", "id":null, "error":{"code":-32700,"message":"Parse error"}}),
        ] {
            let response: Response = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(response).unwrap(), wire);
        }
        let both =
            json!({"jsonrpc":"2.0", "id":1, "result":null, "error":{"code":0,"message":"bad"}});
        assert!(serde_json::from_value::<Response>(both).is_err());
    }
}
