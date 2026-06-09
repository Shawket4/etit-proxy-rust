//! Server-Sent Events stream of live vehicle status.
//!
//! Protocol:
//!   - On connect: a single `snapshot` event with the current `LiveSnapshot`
//!     (so clients render immediately without waiting up to a minute).
//!   - On every Tier B refresh: an `update` event carrying the new
//!     snapshot. Lagging clients (slow networks etc.) drop the oldest
//!     events but never block the publisher.
//!   - Every 30s while idle: a `ping` comment to keep middleboxes from
//!     killing the TCP connection.
//!
//! Clients should reconnect on close — `EventSource` does this automatically.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{self, Stream, StreamExt};
use serde::Serialize;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

use crate::cache::memory::LiveSnapshot;
use crate::domain::LiveVehicleStatus;
use crate::services::broadcaster::LiveEvent;
use crate::state::AppState;

/// `GET /api/v1/stream/live`
pub async fn live_sse(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Initial event: current snapshot from the cache. We always emit *some*
    // snapshot — empty arrays are a valid "no data yet" state.
    let snap = s.cache.live_snapshot();
    let initial = make_event("snapshot", &snapshot_to_payload(&snap))
        .unwrap_or_else(|_| Event::default().event("snapshot").data("[]"));
    let initial_stream = stream::once(async move { Ok(initial) });

    // Subsequent updates: subscribe to the broadcaster.
    let rx = s.broadcaster.subscribe();
    let updates = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(LiveEvent::Update(snap)) => {
                Some(Ok(make_event("update", &snapshot_to_payload(&snap)).unwrap_or_else(|_| {
                    Event::default().event("update").data("[]")
                })))
            }
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                // Inform the client they missed `n` events.
                let payload = serde_json::json!({"missed": n});
                Some(Ok(make_event("lag", &payload).unwrap_or_else(|_| {
                    Event::default().event("lag").data("{}")
                })))
            }
        }
    });

    let body = initial_stream.chain(updates);

    Sse::new(body).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("ping"),
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LivePayload<'a> {
    /// The full vehicle list, sorted by id for stable React diffing.
    vehicles: Vec<&'a LiveVehicleStatus>,
    /// Number of vehicles in this snapshot.
    count: usize,
}

fn snapshot_to_payload(snap: &LiveSnapshot) -> serde_json::Value {
    let mut sorted: Vec<&LiveVehicleStatus> = snap.by_id.values().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let payload = LivePayload {
        count: sorted.len(),
        vehicles: sorted,
    };
    serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null)
}

fn make_event(name: &str, payload: &serde_json::Value) -> Result<Event, serde_json::Error> {
    let data = serde_json::to_string(payload)?;
    Ok(Event::default().event(name).data(data))
}
