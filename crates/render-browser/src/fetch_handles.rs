//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use render_browser::cache::CacheEpoch;
use render_net::CancelToken;
use render_net::FetchError;
use render_net::FetchRequest;
use render_net::FetchResponse;
use render_net::FetchResult;
use render_net::RequestHandle;
use render_net::queue_full_message;
use std::sync::mpsc::TryRecvError;

/// A request handle that can resolve immediately from the private browser
/// cache, asynchronously from the bounded transport worker, or be held back
/// as deferred work while the worker's command queue is momentarily full.
#[derive(Debug)]
pub(super) enum CachedRequestState {
    Ready(Box<Option<FetchResult>>),
    Pending(RequestHandle<FetchResult>),
    /// The network worker rejected the submission because its command queue
    /// was full. The request is parked here and resubmitted from the event
    /// loop's polling pass (`retry_deferred`) — real backpressure that never
    /// blocks the UI thread.
    Deferred(Box<DeferredSubmission>),
}

/// A request parked because the network worker's queue was full.
#[derive(Debug)]
pub(super) struct DeferredSubmission {
    pub(super) request: FetchRequest,
    pub(super) cancel: CancelToken,
}

/// Request metadata travels with its response so a late completion can only
/// update the cache generation that originally submitted it.
#[derive(Debug)]
pub(super) struct CachedRequestHandle {
    pub(super) request: FetchRequest,
    pub(super) epoch: CacheEpoch,
    pub(super) state: CachedRequestState,
}

impl CachedRequestHandle {
    pub(super) fn ready(request: FetchRequest, epoch: CacheEpoch, response: FetchResponse) -> Self {
        Self {
            request,
            epoch,
            state: CachedRequestState::Ready(Box::new(Some(Ok(response)))),
        }
    }

    pub(super) fn pending(
        request: FetchRequest,
        epoch: CacheEpoch,
        handle: RequestHandle<FetchResult>,
    ) -> Self {
        Self {
            request,
            epoch,
            state: CachedRequestState::Pending(handle),
        }
    }

    /// Parks the request as deferred work for a later resubmission.
    pub(super) fn deferred(request: FetchRequest, epoch: CacheEpoch) -> Self {
        let parked_request = request.clone();
        Self {
            request,
            epoch,
            state: CachedRequestState::Deferred(Box::new(DeferredSubmission {
                request: parked_request,
                cancel: CancelToken::default(),
            })),
        }
    }

    pub(super) fn cancel(&self) {
        match &self.state {
            CachedRequestState::Pending(handle) => handle.cancel(),
            CachedRequestState::Deferred(deferred) => deferred.cancel.cancel(),
            CachedRequestState::Ready(_) => {}
        }
    }

    pub(super) fn try_recv(&mut self) -> Result<CachedFetchResult, TryRecvError> {
        let result = match &mut self.state {
            CachedRequestState::Ready(value) => value.take().ok_or(TryRecvError::Disconnected)?,
            // Deferred work stays pending from the receiver's point of view;
            // it only becomes receivable after `retry_deferred` resubmits it.
            CachedRequestState::Deferred(_) => return Err(TryRecvError::Empty),
            CachedRequestState::Pending(handle) => match handle.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return Err(TryRecvError::Empty),
                Err(TryRecvError::Disconnected) => Err(FetchError::WorkerStopped),
            },
        };
        Ok(CachedFetchResult {
            request: self.request.clone(),
            epoch: self.epoch,
            from_cache: matches!(self.state, CachedRequestState::Ready(_)),
            result,
        })
    }

    /// Resubmits a deferred request through `submit`, which maps a request and
    /// its cancellation token onto a live transfer handle.
    ///
    /// Returns whether the request left the deferred state. A resubmission can
    /// bounce straight back to deferred when the queue filled again between
    /// polling passes; that keeps the backpressure honest instead of dropping
    /// the request or failing it with a bogus transport error.
    pub(super) fn retry_deferred(
        &mut self,
        submit: &mut dyn FnMut(FetchRequest, CancelToken) -> RequestHandle<FetchResult>,
    ) -> bool {
        let CachedRequestState::Deferred(deferred) = &mut self.state else {
            return false;
        };
        if deferred.cancel.is_cancelled() {
            self.state = CachedRequestState::Ready(Box::new(Some(Err(FetchError::Cancelled))));
            return true;
        }
        let handle = submit(deferred.request.clone(), deferred.cancel.clone());
        // A saturated queue rejects synchronously; park the request again so
        // the next polling pass retries it.
        self.state = match handle.try_recv() {
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                CachedRequestState::Pending(handle)
            }
            Ok(Err(FetchError::Transport(message))) if message == queue_full_message() => {
                CachedRequestState::Deferred(Box::new(DeferredSubmission {
                    request: deferred.request.clone(),
                    cancel: deferred.cancel.clone(),
                }))
            }
            Ok(result) => CachedRequestState::Ready(Box::new(Some(result))),
        };
        true
    }
}

#[derive(Debug)]
pub(super) struct CachedBatchHandle {
    pub(super) handles: Vec<Option<CachedRequestHandle>>,
    pub(super) results: Vec<Option<CachedFetchResult>>,
}

impl CachedBatchHandle {
    pub(super) fn new(handles: Vec<CachedRequestHandle>) -> Self {
        let len = handles.len();
        Self {
            handles: handles.into_iter().map(Some).collect(),
            results: std::iter::repeat_with(|| None).take(len).collect(),
        }
    }

    pub(super) fn cancel(&self) {
        for handle in self.handles.iter().flatten() {
            handle.cancel();
        }
    }

    /// Resubmits every deferred request in the batch. Returns how many left
    /// the deferred state.
    pub(super) fn retry_deferred(
        &mut self,
        submit: &mut dyn FnMut(FetchRequest, CancelToken) -> RequestHandle<FetchResult>,
    ) -> usize {
        let mut resubmitted = 0;
        for handle in self.handles.iter_mut().flatten() {
            if handle.retry_deferred(submit) {
                resubmitted += 1;
            }
        }
        resubmitted
    }

    pub(super) fn try_recv(&mut self) -> Result<Vec<CachedFetchResult>, TryRecvError> {
        let mut pending = false;
        for index in 0..self.handles.len() {
            let Some(handle) = self.handles[index].as_mut() else {
                continue;
            };
            match handle.try_recv() {
                Ok(result) => {
                    self.results[index] = Some(result);
                    self.handles[index] = None;
                }
                Err(TryRecvError::Empty) => pending = true,
                Err(TryRecvError::Disconnected) => unreachable!("cache handle maps disconnects"),
            }
        }
        if pending {
            return Err(TryRecvError::Empty);
        }
        Ok(self
            .results
            .iter_mut()
            .map(|result| result.take().expect("completed batch result"))
            .collect())
    }
}

#[derive(Debug)]
pub(super) struct CachedFetchResult {
    pub(super) request: FetchRequest,
    pub(super) epoch: CacheEpoch,
    pub(super) from_cache: bool,
    pub(super) result: FetchResult,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use render_net::FetchConfig;
    use render_net::HttpTransport;
    use render_net::NetworkWorker;
    use render_net::Url;

    use super::CachedRequestHandle;
    use super::CachedRequestState;
    use render_browser::cache::CacheEpoch;

    fn test_url(path: &str) -> Url {
        Url::parse(&format!("https://deferred.test{path}")).expect("test URL")
    }

    #[test]
    fn deferred_handle_resubmits_and_completes_through_a_data_url() {
        let worker = NetworkWorker::start(HttpTransport::new(FetchConfig::default()))
            .expect("network worker starts");
        let request = render_net::FetchRequest::get(
            Url::parse("data:text/plain,deferred-body").expect("data URL"),
        );
        let mut handle = CachedRequestHandle::deferred(request, CacheEpoch::default());
        assert!(
            handle.try_recv().is_err(),
            "a deferred handle must stay pending until it is resubmitted"
        );

        let resubmitted = handle.retry_deferred(&mut |request, cancel| {
            worker.submit_with_cancellation(request, cancel)
        });
        assert!(resubmitted, "resubmission leaves the deferred state");
        assert!(
            matches!(handle.state, CachedRequestState::Pending(_)),
            "an accepted resubmission is pending"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Ok(result) = handle.try_recv() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "deferred request never completed"
            );
            std::thread::yield_now();
        };
        assert!(
            result.result.is_ok(),
            "deferred request completed: {result:?}"
        );
    }

    #[test]
    fn cancelling_a_deferred_handle_completes_it_as_cancelled_on_retry() {
        let worker = NetworkWorker::start(HttpTransport::new(FetchConfig::default()))
            .expect("network worker starts");
        let mut handle = CachedRequestHandle::deferred(
            render_net::FetchRequest::get(test_url("/cancelled")),
            CacheEpoch::default(),
        );
        handle.cancel();

        let resubmitted = handle.retry_deferred(&mut |request, cancel| {
            worker.submit_with_cancellation(request, cancel)
        });
        assert!(resubmitted);
        let result = handle.try_recv().expect("cancelled handle resolves");
        assert!(
            matches!(result.result, Err(render_net::FetchError::Cancelled)),
            "cancellation must surface as a Cancelled result, not silent drop"
        );
    }
}
