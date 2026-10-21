use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::StreamEvent;

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
