//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use render_browser::cache::CacheEpoch;
use render_net::FetchError;
use render_net::FetchRequest;
use render_net::FetchResponse;
use render_net::FetchResult;
use render_net::RequestHandle;
use std::sync::mpsc::TryRecvError;

/// A request handle that can resolve immediately from the private browser
/// cache or asynchronously from the bounded transport worker.
#[derive(Debug)]
pub(super) enum CachedRequestState {
    Ready(Box<Option<FetchResult>>),
    Pending(RequestHandle<FetchResult>),
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

    pub(super) fn cancel(&self) {
        if let CachedRequestState::Pending(handle) = &self.state {
            handle.cancel();
        }
    }

    pub(super) fn try_recv(&mut self) -> Result<CachedFetchResult, TryRecvError> {
        let result = match &mut self.state {
            CachedRequestState::Ready(value) => value.take().ok_or(TryRecvError::Disconnected)?,
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
