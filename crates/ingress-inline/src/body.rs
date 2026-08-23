// SPDX-License-Identifier: Apache-2.0
//! Idle-timeout wrapper around the upstream body: frames are passed through
//! untouched; a stall longer than the timeout ends the body with an error.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::{Body, Frame};
use hyper::body::Incoming;
use tokio::time::{Instant, Sleep};

/// Body that errors when the upstream stalls.
pub struct IdleTimeoutBody {
    inner: Incoming,
    timeout: Duration,
    sleep: Pin<Box<Sleep>>,
}

impl IdleTimeoutBody {
    /// Wrap an upstream body.
    #[must_use]
    pub fn new(inner: Incoming, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            sleep: Box::pin(tokio::time::sleep(timeout)),
        }
    }
}

impl Body for IdleTimeoutBody {
    type Data = Bytes;
    type Error = axum::BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let deadline = Instant::now() + self.timeout;
                self.sleep.as_mut().reset(deadline);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(Box::new(e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                if self.sleep.as_mut().poll(cx).is_ready() {
                    tracing::warn!(timeout = ?self.timeout, "upstream stream idle timeout");
                    return Poll::Ready(Some(Err("upstream stream idle timeout".into())));
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
