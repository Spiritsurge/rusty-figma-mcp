//! Wire types for the Host Link Protocol.
//!
//! The central design constraint (§7) is that a host `result` is **never
//! deserialized** — it is captured as a raw JSON fragment and forwarded
//! verbatim. Every type here exists to parse the *envelope* around a payload
//! while leaving the payload itself untouched.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Protocol major version, sent as `?v=` on connect and echoed by `/hello`.
pub const VERSION: u32 = 1;

/// Method name for the progress notification (§6).
pub const PROGRESS_METHOD: &str = "$/progress";

// ---------------------------------------------------------------------------
// Error codes (§3)
// ---------------------------------------------------------------------------

pub mod codes {
    /// Host does not implement the requested method.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Params failed the host's own validation.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Host threw; the message carries the host's own text.
    pub const HOST_ERROR: i32 = -32000;
    /// Deadline exceeded. Synthesised server-side; a host never sends this.
    pub const DEADLINE_EXCEEDED: i32 = -32001;
    /// Host disconnected while the request was in flight.
    pub const DISCONNECTED: i32 = -32002;
    /// Target node, page or style does not exist.
    pub const NOT_FOUND: i32 = -32004;
}

// ---------------------------------------------------------------------------
// Server -> host
// ---------------------------------------------------------------------------

/// A request sent to the host. Serialized directly onto the socket.
#[derive(Debug, Serialize)]
pub struct Request<'a> {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<&'a RawValue>,
}

impl<'a> Request<'a> {
    pub fn new(id: u64, method: &'a str, params: Option<&'a RawValue>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            method,
            params,
        }
    }
}

/// Serializes as the literal `"2.0"`, and refuses to deserialize anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <&str>::deserialize(d)?;
        if raw == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(raw),
                &"\"2.0\"",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Host -> server
// ---------------------------------------------------------------------------

/// One frame received from the host, before it is classified.
///
/// The shape is deliberately permissive: a frame is a response if it carries an
/// `id`, and a notification otherwise. `result` borrows from the input buffer so
/// that a 100 MB document never becomes an object graph.
#[derive(Debug, Deserialize)]
pub struct Incoming<'a> {
    #[allow(dead_code)]
    pub jsonrpc: JsonRpcVersion,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<&'a str>,
    /// Present-but-null must stay distinguishable from absent: `result: null`
    /// is a *successful* response, and many host mutations return exactly that.
    /// Plain `Option` collapses both to `None`, so presence is captured by the
    /// deserializer instead of by serde's null handling.
    #[serde(default, borrow, deserialize_with = "present_raw")]
    pub result: Option<&'a RawValue>,
    #[serde(default)]
    pub error: Option<ErrorObject>,
    #[serde(default, borrow, deserialize_with = "present_raw")]
    pub params: Option<&'a RawValue>,
}

/// Deserializes a present field — including an explicit `null` — as `Some`.
/// Absence is handled by `#[serde(default)]`, which never calls this.
fn present_raw<'de, D>(d: D) -> Result<Option<&'de RawValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <&RawValue>::deserialize(d).map(Some)
}

/// A classified frame. Produced by [`Incoming::classify`].
#[derive(Debug)]
pub enum Frame<'a> {
    /// A response to an in-flight request.
    Response { id: u64, outcome: Outcome<'a> },
    /// A `$/progress` notification for an in-flight request.
    Progress(Progress),
    /// A notification this version does not understand. Ignored, not an error —
    /// §9 allows additive changes without a major bump, so a newer plugin may
    /// legitimately send notifications we have never heard of.
    UnknownNotification,
}

/// The two-valued outcome of a request. `result` xor `error`, enforced here so
/// that the rest of the crate never has to consider "both" or "neither".
#[derive(Debug)]
pub enum Outcome<'a> {
    Ok(&'a RawValue),
    Err(ErrorObject),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}

impl ErrorObject {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn deadline_exceeded(method: &str, secs: u64) -> Self {
        Self::new(
            codes::DEADLINE_EXCEEDED,
            format!("{method} exceeded its {secs}s deadline"),
        )
    }

    pub fn disconnected() -> Self {
        Self::new(
            codes::DISCONNECTED,
            "host disconnected while the request was in flight",
        )
    }
}

/// Payload of a `$/progress` notification (§6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    /// The in-flight request this refers to.
    pub id: u64,
    #[serde(default)]
    pub pct: Option<u8>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MalformedFrame {
    #[error("response {id} carried neither result nor error")]
    ResponseWithoutOutcome { id: u64 },
    #[error("response {id} carried both result and error")]
    ResponseWithBoth { id: u64 },
    #[error("$/progress notification had unreadable params: {0}")]
    BadProgressParams(serde_json::Error),
    #[error("$/progress notification had no params")]
    ProgressWithoutParams,
}

impl<'a> Incoming<'a> {
    /// Sort a frame into one of the three shapes the protocol defines.
    ///
    /// A frame carrying an `id` is a response; one without is a notification.
    /// This is the structural distinction §3 chose JSON-RPC for — no optional
    /// field has to be probed to find out what kind of message arrived.
    pub fn classify(self) -> Result<Frame<'a>, MalformedFrame> {
        match self.id {
            Some(id) => match (self.result, self.error) {
                (Some(_), Some(_)) => Err(MalformedFrame::ResponseWithBoth { id }),
                (Some(result), None) => Ok(Frame::Response {
                    id,
                    outcome: Outcome::Ok(result),
                }),
                (None, Some(error)) => Ok(Frame::Response {
                    id,
                    outcome: Outcome::Err(error),
                }),
                (None, None) => Err(MalformedFrame::ResponseWithoutOutcome { id }),
            },
            None if self.method == Some(PROGRESS_METHOD) => {
                let params = self.params.ok_or(MalformedFrame::ProgressWithoutParams)?;
                let progress = serde_json::from_str(params.get())
                    .map_err(MalformedFrame::BadProgressParams)?;
                Ok(Frame::Progress(progress))
            }
            None => Ok(Frame::UnknownNotification),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(json: &str) -> Result<Frame<'_>, MalformedFrame> {
        serde_json::from_str::<Incoming>(json)
            .expect("valid json")
            .classify()
    }

    #[test]
    fn response_with_result_is_a_response() {
        let frame = classify(r#"{"jsonrpc":"2.0","id":7,"result":{"a":1}}"#).unwrap();
        match frame {
            Frame::Response {
                id: 7,
                outcome: Outcome::Ok(v),
            } => {
                assert_eq!(v.get(), r#"{"a":1}"#);
            }
            other => panic!("expected an Ok response, got {other:?}"),
        }
    }

    #[test]
    fn response_with_error_is_a_response() {
        let frame =
            classify(r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32004,"message":"gone"}}"#)
                .unwrap();
        match frame {
            Frame::Response {
                id: 7,
                outcome: Outcome::Err(e),
            } => {
                assert_eq!(e.code, codes::NOT_FOUND);
                assert_eq!(e.message, "gone");
            }
            other => panic!("expected an Err response, got {other:?}"),
        }
    }

    /// A null result is a *successful* response, not a missing one. Several
    /// Figma mutations legitimately return nothing.
    #[test]
    fn null_result_is_success_not_absence() {
        let frame = classify(r#"{"jsonrpc":"2.0","id":1,"result":null}"#).unwrap();
        assert!(matches!(
            frame,
            Frame::Response {
                outcome: Outcome::Ok(_),
                ..
            }
        ));
    }

    #[test]
    fn progress_is_a_notification() {
        let frame = classify(
            r#"{"jsonrpc":"2.0","method":"$/progress","params":{"id":9,"pct":40,"note":"x"}}"#,
        )
        .unwrap();
        match frame {
            Frame::Progress(p) => {
                assert_eq!(p.id, 9);
                assert_eq!(p.pct, Some(40));
                assert_eq!(p.note.as_deref(), Some("x"));
            }
            other => panic!("expected progress, got {other:?}"),
        }
    }

    /// §9: additive changes must not break an older server.
    #[test]
    fn unknown_notification_is_ignored_not_rejected() {
        let frame = classify(r#"{"jsonrpc":"2.0","method":"$/somethingNew","params":{}}"#).unwrap();
        assert!(matches!(frame, Frame::UnknownNotification));
    }

    #[test]
    fn response_with_neither_outcome_is_malformed() {
        let err = classify(r#"{"jsonrpc":"2.0","id":3}"#).unwrap_err();
        assert!(matches!(
            err,
            MalformedFrame::ResponseWithoutOutcome { id: 3 }
        ));
    }

    #[test]
    fn response_with_both_outcomes_is_malformed() {
        let err =
            classify(r#"{"jsonrpc":"2.0","id":3,"result":1,"error":{"code":1,"message":"m"}}"#)
                .unwrap_err();
        assert!(matches!(err, MalformedFrame::ResponseWithBoth { id: 3 }));
    }

    #[test]
    fn wrong_jsonrpc_version_is_refused() {
        assert!(
            serde_json::from_str::<Incoming>(r#"{"jsonrpc":"1.0","id":1,"result":1}"#).is_err()
        );
    }

    /// §7: the payload must survive as bytes, not be re-serialized through a
    /// `Value`. Key order and number formatting are preserved exactly.
    #[test]
    fn result_is_borrowed_verbatim() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"z":1,"a":[1.50,2],"n":{"deep":true}}}"#;
        let frame = classify(json).unwrap();
        match frame {
            Frame::Response {
                outcome: Outcome::Ok(v),
                ..
            } => {
                assert_eq!(v.get(), r#"{"z":1,"a":[1.50,2],"n":{"deep":true}}"#);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn request_serializes_to_the_documented_shape() {
        let params = serde_json::value::RawValue::from_string(r#"{"depth":3}"#.into()).unwrap();
        let req = Request::new(42, "figma/getDocument", Some(&params));
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"jsonrpc":"2.0","id":42,"method":"figma/getDocument","params":{"depth":3}}"#
        );
    }

    #[test]
    fn request_without_params_omits_the_field() {
        let req = Request::new(1, "figma/getSelection", None);
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"method":"figma/getSelection"}"#
        );
    }
}
