//! Server-Sent Events framing for the streaming plan endpoint.
//!
//! The orchestrator pushes `StreamEvent` values through an mpsc channel; this
//! module serialises each one as a named SSE event and adds keep-alives so
//! proxies do not drop long tool runs. Event names are `stage`, `intent`,
//! `tool_call`, `tool_result`, `plan`, `validation`, `usage`, `refusal` and
//! `done`.

use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::StreamEvent;

/// Streams orchestrator events as SSE, JSON-encoding each event payload.
///
/// A serialisation failure becomes a comment event instead of aborting the
/// stream, so the client still receives the terminal `done` event.
pub fn plan_stream(
    receiver: tokio::sync::mpsc::Receiver<StreamEvent>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let stream = ReceiverStream::new(receiver).map(|event| {
        let sse = Event::default()
            .event(event.event)
            .json_data(event.data)
            .unwrap_or_else(|_| Event::default().comment("serialize error"));
        Ok::<_, Infallible>(sse)
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
