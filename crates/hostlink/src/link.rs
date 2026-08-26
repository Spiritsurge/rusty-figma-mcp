//! Request correlation over a single host connection. See `PROTOCOL.md` §6, §7.
//!
//! One [`Link`] fronts at most one connected host (§5). Requests are matched to
//! responses by id, deadlines are extended by `$/progress`, and payloads reach
//! the caller without ever being parsed.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde_json::value::RawValue;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::protocol::{ErrorObject, Frame, Incoming, Outcome, Request, codes};

/// A host result, held as the frame that carried it plus the span the result
/// occupies within it.
///
/// §7 requires that a result never be deserialized. Holding a span rather than
/// a re-serialized copy means a 100 MB document is read off the socket once and
/// then only ever borrowed — no object graph, and no second buffer.
#[derive(Debug, Clone)]
pub struct Payload {
    frame: Arc<str>,
    span: Range<usize>,
}

impl Payload {
    /// Capture the span `raw` occupies within `frame`.
    ///
    /// `raw` must borrow from `frame`, which is guaranteed by the only caller:
    /// it parses `frame` and immediately captures, before the borrow ends.
    fn capture(frame: Arc<str>, raw: &RawValue) -> Self {
        let base = frame.as_ptr() as usize;
        let start = raw.get().as_ptr() as usize - base;
        debug_assert!(start + raw.get().len() <= frame.len(), "raw must borrow from frame");
        Self { span: start..start + raw.get().len(), frame }
    }

    /// The result as it arrived on the wire: original key order, original
    /// number formatting, no round trip.
    pub fn get(&self) -> &str {
        &self.frame[self.span.clone()]
    }

    /// Bytes of payload, for logging and metrics.
    pub fn len(&self) -> usize {
        self.span.len()
    }

    pub fn is_empty(&self) -> bool {
        self.span.is_empty()
    }
}

pub type Reply = Result<Payload, ErrorObject>;

struct Pending {
    reply: oneshot::Sender<Reply>,
    /// Extended by `$/progress`; watched by the waiting request (§6).
    deadline: watch::Sender<Instant>,
    /// The request's full interval. §6 resets to this on progress rather than
    /// topping up whatever remained, so a host reporting steadily stays alive
    /// indefinitely while one that goes quiet still expires on schedule.
    interval: Duration,
    method: String,
}

/// Fronts one host connection and correlates requests to responses.
#[derive(Clone, Default)]
pub struct Link {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    pending: DashMap<u64, Pending>,
    next_id: AtomicU64,
    /// `Some` only while a host is connected (§5).
    outbound: Mutex<Option<mpsc::UnboundedSender<String>>>,
}

impl Link {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn is_connected(&self) -> bool {
        self.inner.outbound.lock().await.is_some()
    }

    /// Send a request and wait for its response.
    ///
    /// Returns `Err` with a protocol error code when the host reports failure,
    /// disconnects mid-flight (-32002), or the deadline expires (-32001).
    pub async fn request(
        &self,
        method: &str,
        params: Option<&RawValue>,
        timeout: Duration,
    ) -> Reply {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;

        let body = serde_json::to_string(&Request::new(id, method, params))
            .map_err(|e| ErrorObject::new(codes::HOST_ERROR, format!("serialize request: {e}")))?;

        let (reply_tx, mut reply_rx) = oneshot::channel();
        let (deadline_tx, mut deadline_rx) = watch::channel(Instant::now() + timeout);

        self.inner.pending.insert(
            id,
            Pending {
                reply: reply_tx,
                deadline: deadline_tx,
                interval: timeout,
                method: method.to_owned(),
            },
        );

        // Registered before sending, so a response cannot arrive before there is
        // somewhere to put it.
        if let Err(e) = self.write(body).await {
            self.inner.pending.remove(&id);
            return Err(e);
        }

        debug!(id, method, "sent");

        let outcome = loop {
            let deadline = *deadline_rx.borrow_and_update();

            tokio::select! {
                biased;

                reply = &mut reply_rx => {
                    break reply.unwrap_or_else(|_| Err(ErrorObject::disconnected()));
                }

                // A progress notification moved the deadline; recompute and wait again.
                _ = deadline_rx.changed() => continue,

                _ = tokio::time::sleep_until(deadline) => {
                    // The deadline may have been extended between the sleep
                    // resolving and this arm running; only give up if it stands.
                    if *deadline_rx.borrow() > deadline {
                        continue;
                    }
                    warn!(id, method, "deadline exceeded");
                    break Err(ErrorObject::deadline_exceeded(method, timeout.as_secs()));
                }
            }
        };

        // A late response has nowhere to go, which the reader treats as normal.
        self.inner.pending.remove(&id);
        outcome
    }

    async fn write(&self, body: String) -> Result<(), ErrorObject> {
        let guard = self.inner.outbound.lock().await;
        let tx = guard
            .as_ref()
            .ok_or_else(|| ErrorObject::new(codes::DISCONNECTED, "no host is connected"))?;
        tx.send(body).map_err(|_| ErrorObject::disconnected())
    }

    /// Install an outbound channel for a newly connected host, returning the
    /// receiver the writer task should drain.
    ///
    /// Replaces any existing connection (§5): a plugin reload is
    /// indistinguishable from a second connection, so the newest wins.
    pub(crate) async fn connect(&self) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let replaced = self.inner.outbound.lock().await.replace(tx).is_some();
        if replaced {
            self.fail_all(ErrorObject::disconnected());
        }
        rx
    }

    /// Tear down the current connection and fail everything still in flight.
    pub(crate) async fn disconnect(&self) {
        self.inner.outbound.lock().await.take();
        self.fail_all(ErrorObject::disconnected());
    }

    fn fail_all(&self, err: ErrorObject) {
        let ids: Vec<u64> = self.inner.pending.iter().map(|e| *e.key()).collect();
        for id in ids {
            if let Some((_, pending)) = self.inner.pending.remove(&id) {
                let _ = pending.reply.send(Err(err.clone()));
            }
        }
    }

    /// Route one frame received from the host.
    ///
    /// Kept free of any socket type so the correlation logic is testable
    /// without standing up a WebSocket.
    pub(crate) fn handle_frame(&self, frame: String) {
        let frame: Arc<str> = Arc::from(frame);

        let incoming: Incoming = match serde_json::from_str(&frame) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "discarding unparseable frame");
                return;
            }
        };

        let classified = match incoming.classify() {
            Ok(f) => f,
            Err(e) => {
                warn!(error = %e, "discarding malformed frame");
                return;
            }
        };

        match classified {
            Frame::Response { id, outcome } => {
                // Capture the span before the borrow on `frame` ends.
                let reply: Reply = match outcome {
                    Outcome::Ok(raw) => Ok(Payload::capture(Arc::clone(&frame), raw)),
                    Outcome::Err(e) => Err(e),
                };

                match self.inner.pending.remove(&id) {
                    Some((_, pending)) => {
                        let _ = pending.reply.send(reply);
                    }
                    // Already timed out or already answered. §6 calls this
                    // normal, so it is logged at debug, not warn.
                    None => debug!(id, "response for an unknown request; discarded"),
                }
            }

            Frame::Progress(p) => match self.inner.pending.get(&p.id) {
                Some(pending) => {
                    let _ = pending.deadline.send(Instant::now() + pending.interval);
                    debug!(id = p.id, pct = ?p.pct, note = ?p.note, method = %pending.method, "progress");
                }
                None => debug!(id = p.id, "progress for an unknown request; ignored"),
            },

            Frame::UnknownNotification => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_owned()).unwrap()
    }

    /// Drive a link with a connected host whose frames the test supplies.
    async fn connected() -> (Link, mpsc::UnboundedReceiver<String>) {
        let link = Link::new();
        let rx = link.connect().await;
        (link, rx)
    }

    #[tokio::test]
    async fn request_resolves_with_its_payload() {
        let (link, mut outbound) = connected().await;

        let task = {
            let link = link.clone();
            tokio::spawn(async move {
                link.request("figma/getNode", None, Duration::from_secs(5)).await
            })
        };

        let sent = outbound.recv().await.expect("request written");
        assert!(sent.contains(r#""method":"figma/getNode""#));
        assert!(sent.contains(r#""id":1"#));

        link.handle_frame(r#"{"jsonrpc":"2.0","id":1,"result":{"name":"Frame 1"}}"#.into());

        let payload = task.await.unwrap().expect("ok");
        assert_eq!(payload.get(), r#"{"name":"Frame 1"}"#);
    }

    #[tokio::test]
    async fn host_error_reaches_the_caller() {
        let (link, mut outbound) = connected().await;
        let task = {
            let link = link.clone();
            tokio::spawn(async move { link.request("figma/getNode", None, Duration::from_secs(5)).await })
        };
        outbound.recv().await.unwrap();

        link.handle_frame(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no such node"}}"#.into(),
        );

        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(err.message, "no such node");
    }

    /// Ids must not be reused across concurrent requests, and each response
    /// must reach its own waiter.
    #[tokio::test]
    async fn concurrent_requests_do_not_cross() {
        let (link, mut outbound) = connected().await;

        let a = {
            let link = link.clone();
            tokio::spawn(async move { link.request("a", None, Duration::from_secs(5)).await })
        };
        let first = outbound.recv().await.unwrap();
        let b = {
            let link = link.clone();
            tokio::spawn(async move { link.request("b", None, Duration::from_secs(5)).await })
        };
        let second = outbound.recv().await.unwrap();

        assert!(first.contains(r#""id":1"#), "got {first}");
        assert!(second.contains(r#""id":2"#), "got {second}");

        // Answer out of order.
        link.handle_frame(r#"{"jsonrpc":"2.0","id":2,"result":"B"}"#.into());
        link.handle_frame(r#"{"jsonrpc":"2.0","id":1,"result":"A"}"#.into());

        assert_eq!(a.await.unwrap().unwrap().get(), r#""A""#);
        assert_eq!(b.await.unwrap().unwrap().get(), r#""B""#);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expires_without_progress() {
        let (link, mut outbound) = connected().await;
        let task = {
            let link = link.clone();
            tokio::spawn(async move { link.request("slow", None, Duration::from_secs(30)).await })
        };
        outbound.recv().await.unwrap();

        tokio::time::advance(Duration::from_secs(31)).await;

        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.code, codes::DEADLINE_EXCEEDED);
    }

    /// §6: progress must keep a long operation alive past its original deadline.
    #[tokio::test(start_paused = true)]
    async fn progress_extends_the_deadline() {
        let (link, mut outbound) = connected().await;
        let task = {
            let link = link.clone();
            tokio::spawn(async move { link.request("slow", None, Duration::from_secs(30)).await })
        };
        outbound.recv().await.unwrap();

        // Report progress every 20s across a span far beyond the original 30s.
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(20)).await;
            tokio::task::yield_now().await;
            link.handle_frame(
                r#"{"jsonrpc":"2.0","method":"$/progress","params":{"id":1,"pct":50}}"#.into(),
            );
            tokio::task::yield_now().await;
        }

        assert!(!task.is_finished(), "progress should have kept the request alive");

        link.handle_frame(r#"{"jsonrpc":"2.0","id":1,"result":"done"}"#.into());
        assert_eq!(task.await.unwrap().unwrap().get(), r#""done""#);
    }

    #[tokio::test]
    async fn disconnect_fails_everything_in_flight() {
        let (link, mut outbound) = connected().await;
        let task = {
            let link = link.clone();
            tokio::spawn(async move { link.request("x", None, Duration::from_secs(30)).await })
        };
        outbound.recv().await.unwrap();

        link.disconnect().await;

        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.code, codes::DISCONNECTED);
    }

    #[tokio::test]
    async fn request_without_a_host_fails_immediately() {
        let link = Link::new();
        let err = link
            .request("x", None, Duration::from_secs(30))
            .await
            .unwrap_err();
        assert_eq!(err.code, codes::DISCONNECTED);
    }

    /// A response arriving after its request gave up must not panic or leak.
    #[tokio::test(start_paused = true)]
    async fn late_response_is_discarded() {
        let (link, mut outbound) = connected().await;
        let task = {
            let link = link.clone();
            tokio::spawn(async move { link.request("x", None, Duration::from_secs(30)).await })
        };
        outbound.recv().await.unwrap();

        tokio::time::advance(Duration::from_secs(31)).await;
        assert_eq!(task.await.unwrap().unwrap_err().code, codes::DEADLINE_EXCEEDED);

        link.handle_frame(r#"{"jsonrpc":"2.0","id":1,"result":"too late"}"#.into());
        assert_eq!(link.inner.pending.len(), 0);
    }

    #[tokio::test]
    async fn garbage_frames_are_survivable() {
        let (link, _outbound) = connected().await;
        link.handle_frame("not json at all".into());
        link.handle_frame(r#"{"jsonrpc":"2.0","id":1}"#.into());
        link.handle_frame(r#"{"jsonrpc":"9.9","id":1,"result":1}"#.into());
        link.handle_frame(r#"{"jsonrpc":"2.0","method":"$/progress"}"#.into());
        assert_eq!(link.inner.pending.len(), 0);
    }

    #[tokio::test]
    async fn params_are_forwarded_verbatim() {
        let (link, mut outbound) = connected().await;
        let p = raw(r#"{"depth":3,"nodeId":"1:23"}"#);
        let task = {
            let link = link.clone();
            tokio::spawn(async move {
                link.request("figma/getDocument", Some(&p), Duration::from_secs(5)).await
            })
        };
        let sent = outbound.recv().await.unwrap();
        assert_eq!(
            sent,
            r#"{"jsonrpc":"2.0","id":1,"method":"figma/getDocument","params":{"depth":3,"nodeId":"1:23"}}"#
        );
        link.handle_frame(r#"{"jsonrpc":"2.0","id":1,"result":null}"#.into());
        assert_eq!(task.await.unwrap().unwrap().get(), "null");
    }

    /// The payload must be the original bytes, not a serde round trip.
    #[tokio::test]
    async fn payload_preserves_key_order_and_number_format() {
        let (link, mut outbound) = connected().await;
        let task = {
            let link = link.clone();
            tokio::spawn(async move { link.request("x", None, Duration::from_secs(5)).await })
        };
        outbound.recv().await.unwrap();

        let odd = r#"{"z":1,"a":2,"f":1.50,"big":12345678901234567890}"#;
        link.handle_frame(format!(r#"{{"jsonrpc":"2.0","id":1,"result":{odd}}}"#));

        assert_eq!(task.await.unwrap().unwrap().get(), odd);
    }
}
