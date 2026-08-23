//! Deterministic task, microtask, timer, and rendering scheduling.
//!
//! The scheduler deliberately stores opaque host payloads instead of Rust
//! callbacks. A JavaScript runtime, browser shell, or deterministic test runner
//! decides how to execute a payload, while this module owns the HTML event-loop
//! ordering rules and resource bounds.

use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::time::Duration;

use crate::dom::{Dom, DomRevision, MutationBatch, MutationHistoryError};

/// The specification-defined source that placed a task on the event loop.
///
/// Sources remain explicit even though the deterministic reference scheduler
/// currently selects ready tasks in global FIFO order. This leaves room for a
/// browser scheduler to prioritize between sources without changing payloads
/// or weakening FIFO order within a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TaskSource {
    DomManipulation,
    UserInteraction,
    Networking,
    HistoryTraversal,
    PostedMessage,
    Timer,
    PerformanceTimeline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MicrotaskId(u64);

impl MicrotaskId {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId(u64);

impl TimerId {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Bounds memory use and prevents an unbounded microtask checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventLoopLimits {
    /// Maximum ready tasks plus timers that have not executed yet.
    pub max_pending_tasks: usize,
    /// Maximum timers waiting for their deadline.
    pub max_pending_timers: usize,
    pub max_pending_microtasks: usize,
    /// Maximum jobs executed by one microtask checkpoint.
    pub max_microtasks_per_checkpoint: usize,
}

impl Default for EventLoopLimits {
    fn default() -> Self {
        Self {
            max_pending_tasks: 4_096,
            max_pending_timers: 4_096,
            max_pending_microtasks: 4_096,
            max_microtasks_per_checkpoint: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    TaskCapacityReached { limit: usize },
    TimerCapacityReached { limit: usize },
    MicrotaskCapacityReached { limit: usize },
    IdentifierSpaceExhausted,
    TimerDeadlineOverflow,
}

impl fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskCapacityReached { limit } => {
                write!(formatter, "pending task limit of {limit} reached")
            }
            Self::TimerCapacityReached { limit } => {
                write!(formatter, "pending timer limit of {limit} reached")
            }
            Self::MicrotaskCapacityReached { limit } => {
                write!(formatter, "pending microtask limit of {limit} reached")
            }
            Self::IdentifierSpaceExhausted => {
                formatter.write_str("event-loop identifier space exhausted")
            }
            Self::TimerDeadlineOverflow => {
                formatter.write_str("timer deadline exceeds virtual clock range")
            }
        }
    }
}

impl Error for QueueError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockError {
    WouldMoveBackwards {
        current: Duration,
        requested: Duration,
    },
    Overflow,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WouldMoveBackwards { current, requested } => write!(
                formatter,
                "virtual clock cannot move from {current:?} backwards to {requested:?}"
            ),
            Self::Overflow => formatter.write_str("virtual clock range exceeded"),
        }
    }
}

impl Error for ClockError {}

/// A monotonic clock controlled by the embedding host rather than wall time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtualClock {
    now: Duration,
}

impl VirtualClock {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            now: Duration::ZERO,
        }
    }

    #[must_use]
    pub const fn now(self) -> Duration {
        self.now
    }

    /// Advance the clock by an exact duration.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::Overflow`] if the resulting instant cannot be
    /// represented by [`Duration`].
    pub fn advance_by(&mut self, duration: Duration) -> Result<(), ClockError> {
        self.now = self.now.checked_add(duration).ok_or(ClockError::Overflow)?;
        Ok(())
    }

    /// Advance the clock to an exact instant without permitting time travel.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::WouldMoveBackwards`] when `instant` is earlier
    /// than the current time.
    pub fn advance_to(&mut self, instant: Duration) -> Result<(), ClockError> {
        if instant < self.now {
            return Err(ClockError::WouldMoveBackwards {
                current: self.now,
                requested: instant,
            });
        }
        self.now = instant;
        Ok(())
    }
}

/// A unit of host work selected by the scheduler.
#[derive(Debug, PartialEq, Eq)]
pub enum Runnable<T> {
    Task {
        id: TaskId,
        source: TaskSource,
        payload: T,
    },
    Microtask {
        id: MicrotaskId,
        payload: T,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutedTask {
    pub id: TaskId,
    pub source: TaskSource,
}

/// Result of draining microtasks after one task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicrotaskCheckpoint {
    Complete {
        executed: usize,
    },
    /// The configured execution budget was exhausted. Remaining jobs are
    /// discarded explicitly so they cannot leak into a later task checkpoint.
    ResourceLimitReached {
        executed: usize,
        discarded: usize,
    },
}

/// Work requested at the rendering opportunity after a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderingDecision {
    NoChange,
    Incremental(MutationBatch),
    /// Mutation history no longer covers the rendering cursor. Consumers must
    /// recompute from the current DOM instead of guessing an invalidation set.
    FullRefresh {
        from_revision: DomRevision,
        to_revision: DomRevision,
        cause: MutationHistoryError,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderingOpportunity {
    pub at: Duration,
    pub decision: RenderingDecision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnOutcome {
    pub task: ExecutedTask,
    pub microtasks: MicrotaskCheckpoint,
    pub rendering: RenderingOpportunity,
}

struct ScheduledTask<T> {
    id: TaskId,
    source: TaskSource,
    payload: T,
}

struct ScheduledMicrotask<T> {
    id: MicrotaskId,
    payload: T,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
    deadline: Duration,
    id: TimerId,
}

/// Deterministic reference implementation of an HTML event-loop turn.
pub struct EventLoop<T> {
    limits: EventLoopLimits,
    clock: VirtualClock,
    ready_tasks: VecDeque<ScheduledTask<T>>,
    microtasks: VecDeque<ScheduledMicrotask<T>>,
    timers: BTreeMap<TimerKey, ScheduledTask<T>>,
    next_task_id: u64,
    next_microtask_id: u64,
    next_timer_id: u64,
    rendering_revision: DomRevision,
}

impl<T> EventLoop<T> {
    #[must_use]
    pub fn new(dom: &Dom) -> Self {
        Self::with_limits(dom, EventLoopLimits::default())
    }

    #[must_use]
    pub fn with_limits(dom: &Dom, limits: EventLoopLimits) -> Self {
        Self {
            limits,
            clock: VirtualClock::new(),
            ready_tasks: VecDeque::new(),
            microtasks: VecDeque::new(),
            timers: BTreeMap::new(),
            next_task_id: 0,
            next_microtask_id: 0,
            next_timer_id: 0,
            rendering_revision: dom.revision(),
        }
    }

    #[must_use]
    pub const fn now(&self) -> Duration {
        self.clock.now()
    }

    #[must_use]
    pub const fn limits(&self) -> EventLoopLimits {
        self.limits
    }

    #[must_use]
    pub fn ready_task_count(&self) -> usize {
        self.ready_tasks.len()
    }

    /// Return ready tasks plus timers that have not executed yet.
    #[must_use]
    pub fn pending_task_count(&self) -> usize {
        self.ready_tasks.len().saturating_add(self.timers.len())
    }

    #[must_use]
    pub fn pending_microtask_count(&self) -> usize {
        self.microtasks.len()
    }

    #[must_use]
    pub fn pending_timer_count(&self) -> usize {
        self.timers.len()
    }

    #[must_use]
    pub fn next_timer_deadline(&self) -> Option<Duration> {
        self.timers.first_key_value().map(|(key, _)| key.deadline)
    }

    /// Queue a task at the end of the deterministic ready queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the task resource bound or identifier space is
    /// exhausted.
    pub fn queue_task(&mut self, source: TaskSource, payload: T) -> Result<TaskId, QueueError> {
        self.ensure_task_capacity()?;
        let id = self.allocate_task_id()?;
        self.ready_tasks.push_back(ScheduledTask {
            id,
            source,
            payload,
        });
        Ok(id)
    }

    /// Queue a microtask for the checkpoint following the current task.
    ///
    /// # Errors
    ///
    /// Returns an error when the microtask resource bound or identifier space
    /// is exhausted.
    pub fn queue_microtask(&mut self, payload: T) -> Result<MicrotaskId, QueueError> {
        if self.microtasks.len() >= self.limits.max_pending_microtasks {
            return Err(QueueError::MicrotaskCapacityReached {
                limit: self.limits.max_pending_microtasks,
            });
        }
        let id = self.allocate_microtask_id()?;
        self.microtasks
            .push_back(ScheduledMicrotask { id, payload });
        Ok(id)
    }

    /// Schedule a payload on the timer task source.
    ///
    /// Timers with equal deadlines enter the ready queue in creation order.
    /// Time advances only through [`Self::advance_time_by`] or
    /// [`Self::advance_time_to`].
    ///
    /// # Errors
    ///
    /// Returns an error for exhausted task/timer bounds, exhausted identifier
    /// space, or a deadline outside the virtual clock range.
    pub fn set_timeout(&mut self, delay: Duration, payload: T) -> Result<TimerId, QueueError> {
        self.ensure_task_capacity()?;
        if self.timers.len() >= self.limits.max_pending_timers {
            return Err(QueueError::TimerCapacityReached {
                limit: self.limits.max_pending_timers,
            });
        }
        let deadline = self
            .clock
            .now()
            .checked_add(delay)
            .ok_or(QueueError::TimerDeadlineOverflow)?;
        let task_id = self.allocate_task_id()?;
        let timer_id = self.allocate_timer_id()?;
        self.timers.insert(
            TimerKey {
                deadline,
                id: timer_id,
            },
            ScheduledTask {
                id: task_id,
                source: TaskSource::Timer,
                payload,
            },
        );
        Ok(timer_id)
    }

    /// Remove every pending timer whose payload satisfies `predicate` and
    /// return how many were removed. Fired timers are unaffected.
    pub fn cancel_timers<F>(&mut self, mut predicate: F) -> usize
    where
        F: FnMut(&T) -> bool,
    {
        let before = self.timers.len();
        self.timers
            .retain(|_, scheduled| !predicate(&scheduled.payload));
        before - self.timers.len()
    }

    /// Advance virtual time by an exact duration.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::Overflow`] when the new instant cannot be
    /// represented.
    pub fn advance_time_by(&mut self, duration: Duration) -> Result<(), ClockError> {
        self.clock.advance_by(duration)
    }

    /// Advance virtual time to an exact instant.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::WouldMoveBackwards`] when the requested instant is
    /// earlier than the current clock.
    pub fn advance_time_to(&mut self, instant: Duration) -> Result<(), ClockError> {
        self.clock.advance_to(instant)
    }

    /// Execute one task, drain its microtask checkpoint, then expose one
    /// rendering opportunity. Returns `None` when no task is ready.
    ///
    /// The executor may queue more work and mutate `dom`. Microtasks queued by
    /// either the task or another microtask join the same FIFO checkpoint.
    pub fn run_one_turn<F>(&mut self, dom: &mut Dom, mut execute: F) -> Option<TurnOutcome>
    where
        F: FnMut(Runnable<T>, &mut Self, &mut Dom),
    {
        self.promote_due_timers();
        let task = self.ready_tasks.pop_front()?;
        let executed_task = ExecutedTask {
            id: task.id,
            source: task.source,
        };
        execute(
            Runnable::Task {
                id: task.id,
                source: task.source,
                payload: task.payload,
            },
            self,
            dom,
        );

        let microtasks = self.perform_microtask_checkpoint(dom, &mut execute);
        let rendering = RenderingOpportunity {
            at: self.clock.now(),
            decision: self.rendering_decision(dom),
        };
        Some(TurnOutcome {
            task: executed_task,
            microtasks,
            rendering,
        })
    }

    fn perform_microtask_checkpoint<F>(
        &mut self,
        dom: &mut Dom,
        execute: &mut F,
    ) -> MicrotaskCheckpoint
    where
        F: FnMut(Runnable<T>, &mut Self, &mut Dom),
    {
        let mut executed = 0;
        while let Some(microtask) = self.microtasks.pop_front() {
            if executed >= self.limits.max_microtasks_per_checkpoint {
                let discarded = self.microtasks.len().saturating_add(1);
                self.microtasks.clear();
                return MicrotaskCheckpoint::ResourceLimitReached {
                    executed,
                    discarded,
                };
            }
            execute(
                Runnable::Microtask {
                    id: microtask.id,
                    payload: microtask.payload,
                },
                self,
                dom,
            );
            executed += 1;
        }
        MicrotaskCheckpoint::Complete { executed }
    }

    fn rendering_decision(&mut self, dom: &Dom) -> RenderingDecision {
        match dom.mutations_since(self.rendering_revision) {
            Ok(batch) if batch.records.is_empty() => RenderingDecision::NoChange,
            Ok(batch) => {
                self.rendering_revision = batch.to_revision;
                RenderingDecision::Incremental(batch)
            }
            Err(cause) => {
                let from_revision = self.rendering_revision;
                let to_revision = dom.revision();
                self.rendering_revision = to_revision;
                RenderingDecision::FullRefresh {
                    from_revision,
                    to_revision,
                    cause,
                }
            }
        }
    }

    fn promote_due_timers(&mut self) {
        while let Some((key, _)) = self.timers.first_key_value() {
            if key.deadline > self.clock.now() {
                break;
            }
            let (_, task) = self
                .timers
                .pop_first()
                .expect("the first timer was observed immediately before removal");
            self.ready_tasks.push_back(task);
        }
    }

    fn ensure_task_capacity(&self) -> Result<(), QueueError> {
        if self.pending_task_count() >= self.limits.max_pending_tasks {
            Err(QueueError::TaskCapacityReached {
                limit: self.limits.max_pending_tasks,
            })
        } else {
            Ok(())
        }
    }

    fn allocate_task_id(&mut self) -> Result<TaskId, QueueError> {
        let id = TaskId(self.next_task_id);
        self.next_task_id = self
            .next_task_id
            .checked_add(1)
            .ok_or(QueueError::IdentifierSpaceExhausted)?;
        Ok(id)
    }

    fn allocate_microtask_id(&mut self) -> Result<MicrotaskId, QueueError> {
        let id = MicrotaskId(self.next_microtask_id);
        self.next_microtask_id = self
            .next_microtask_id
            .checked_add(1)
            .ok_or(QueueError::IdentifierSpaceExhausted)?;
        Ok(id)
    }

    fn allocate_timer_id(&mut self) -> Result<TimerId, QueueError> {
        let id = TimerId(self.next_timer_id);
        self.next_timer_id = self
            .next_timer_id
            .checked_add(1)
            .ok_or(QueueError::IdentifierSpaceExhausted)?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClockError, EventLoop, EventLoopLimits, MicrotaskCheckpoint, QueueError, RenderingDecision,
        Runnable, TaskSource,
    };
    use crate::dom::Dom;
    use std::time::Duration;

    #[test]
    fn tasks_and_microtasks_have_deterministic_checkpoint_order() {
        let mut dom = Dom::new();
        let mut event_loop = EventLoop::new(&dom);
        event_loop
            .queue_task(TaskSource::DomManipulation, "task-1")
            .expect("first task fits");
        event_loop
            .queue_task(TaskSource::Networking, "task-2")
            .expect("second task fits");
        let mut log = Vec::new();

        let first = event_loop
            .run_one_turn(&mut dom, |runnable, scheduler, _| {
                let payload = match runnable {
                    Runnable::Task { payload, .. } | Runnable::Microtask { payload, .. } => payload,
                };
                log.push(payload);
                match payload {
                    "task-1" => {
                        scheduler
                            .queue_microtask("microtask-1")
                            .expect("microtask fits");
                        scheduler
                            .queue_microtask("microtask-2")
                            .expect("microtask fits");
                    }
                    "microtask-1" => {
                        scheduler
                            .queue_microtask("microtask-3")
                            .expect("nested microtask fits");
                    }
                    _ => {}
                }
            })
            .expect("first task is ready");

        assert_eq!(log, ["task-1", "microtask-1", "microtask-2", "microtask-3"]);
        assert_eq!(
            first.microtasks,
            MicrotaskCheckpoint::Complete { executed: 3 }
        );
        assert_eq!(first.rendering.decision, RenderingDecision::NoChange);

        event_loop
            .run_one_turn(&mut dom, |runnable, _, _| {
                if let Runnable::Task { payload, .. } = runnable {
                    log.push(payload);
                }
            })
            .expect("second task is ready");
        assert_eq!(log.last(), Some(&"task-2"));
    }

    #[test]
    fn virtual_timers_use_deadline_then_creation_order() {
        let mut dom = Dom::new();
        let mut event_loop = EventLoop::new(&dom);
        event_loop
            .set_timeout(Duration::from_millis(10), "late")
            .expect("timer fits");
        event_loop
            .set_timeout(Duration::from_millis(5), "first-at-five")
            .expect("timer fits");
        event_loop
            .set_timeout(Duration::from_millis(5), "second-at-five")
            .expect("timer fits");

        event_loop
            .advance_time_by(Duration::from_millis(4))
            .expect("clock advances");
        assert!(event_loop.run_one_turn(&mut dom, |_, _, _| {}).is_none());

        event_loop
            .advance_time_by(Duration::from_millis(1))
            .expect("clock advances to timer");
        let mut log = Vec::new();
        for _ in 0..2 {
            let outcome = event_loop
                .run_one_turn(&mut dom, |runnable, _, _| {
                    if let Runnable::Task {
                        source, payload, ..
                    } = runnable
                    {
                        assert_eq!(source, TaskSource::Timer);
                        log.push(payload);
                    }
                })
                .expect("five millisecond timer is ready");
            assert_eq!(outcome.rendering.at, Duration::from_millis(5));
        }
        assert_eq!(log, ["first-at-five", "second-at-five"]);

        event_loop
            .advance_time_to(Duration::from_millis(10))
            .expect("clock advances to final timer");
        event_loop
            .run_one_turn(&mut dom, |runnable, _, _| {
                if let Runnable::Task { payload, .. } = runnable {
                    log.push(payload);
                }
            })
            .expect("ten millisecond timer is ready");
        assert_eq!(log.last(), Some(&"late"));
        assert!(matches!(
            event_loop.advance_time_to(Duration::from_millis(9)),
            Err(ClockError::WouldMoveBackwards { .. })
        ));
    }

    #[test]
    fn resource_limits_are_explicit_and_stop_runaway_checkpoints() {
        let mut dom = Dom::new();
        let limits = EventLoopLimits {
            max_pending_tasks: 3,
            max_pending_timers: 1,
            max_pending_microtasks: 2,
            max_microtasks_per_checkpoint: 1,
        };
        let mut event_loop = EventLoop::with_limits(&dom, limits);
        event_loop
            .queue_task(TaskSource::DomManipulation, "task")
            .expect("task fits");
        event_loop
            .set_timeout(Duration::ZERO, "timer")
            .expect("one timer fits");
        assert_eq!(
            event_loop.set_timeout(Duration::ZERO, "timer-overflow"),
            Err(QueueError::TimerCapacityReached { limit: 1 })
        );
        event_loop
            .queue_task(TaskSource::Networking, "second-task")
            .expect("second ready task fits");
        assert_eq!(
            event_loop.queue_task(TaskSource::Networking, "overflow"),
            Err(QueueError::TaskCapacityReached { limit: 3 })
        );
        event_loop
            .queue_microtask("microtask-1")
            .expect("first microtask fits");
        event_loop
            .queue_microtask("microtask-2")
            .expect("second microtask fits");
        assert_eq!(
            event_loop.queue_microtask("microtask-overflow"),
            Err(QueueError::MicrotaskCapacityReached { limit: 2 })
        );

        let outcome = event_loop
            .run_one_turn(&mut dom, |_, _, _| {})
            .expect("task is ready");
        assert_eq!(
            outcome.microtasks,
            MicrotaskCheckpoint::ResourceLimitReached {
                executed: 1,
                discarded: 1,
            }
        );
        assert_eq!(event_loop.pending_microtask_count(), 0);
    }

    #[test]
    fn dom_mutation_batch_triggers_incremental_rendering_after_microtasks() {
        enum Work {
            QueueMutation,
            MutateDom,
        }

        let mut dom = Dom::new();
        let mut event_loop = EventLoop::new(&dom);
        event_loop
            .queue_task(TaskSource::DomManipulation, Work::QueueMutation)
            .expect("task fits");

        let outcome = event_loop
            .run_one_turn(&mut dom, |runnable, scheduler, dom| match runnable {
                Runnable::Task {
                    payload: Work::QueueMutation,
                    ..
                } => {
                    scheduler
                        .queue_microtask(Work::MutateDom)
                        .expect("mutation microtask fits");
                }
                Runnable::Microtask {
                    payload: Work::MutateDom,
                    ..
                } => {
                    let element = dom.create_element("main");
                    dom.append_child(dom.document(), element)
                        .expect("document accepts its element");
                }
                _ => {}
            })
            .expect("task is ready");

        let RenderingDecision::Incremental(batch) = outcome.rendering.decision else {
            panic!("DOM mutation must request incremental rendering");
        };
        assert_eq!(batch.from_revision.as_u64(), 0);
        assert_eq!(batch.to_revision, dom.revision());
        assert_eq!(batch.records.len(), 1);
        assert!(batch.impact().affects_style());
        assert!(batch.impact().affects_layout());

        event_loop
            .queue_task(TaskSource::PostedMessage, Work::QueueMutation)
            .expect("follow-up task fits");
        let next = event_loop
            .run_one_turn(&mut dom, |_, _, _| {})
            .expect("follow-up task is ready");
        assert_eq!(next.rendering.decision, RenderingDecision::NoChange);
    }

    #[test]
    fn discarded_mutation_history_requests_full_refresh() {
        let mut dom = Dom::new();
        let mut event_loop = EventLoop::new(&dom);
        dom.set_mutation_journal_capacity(0);
        event_loop
            .queue_task(TaskSource::DomManipulation, ())
            .expect("task fits");

        let outcome = event_loop
            .run_one_turn(&mut dom, |_, _, dom| {
                let element = dom.create_element("main");
                dom.append_child(dom.document(), element)
                    .expect("document accepts its element");
            })
            .expect("task is ready");

        assert!(matches!(
            outcome.rendering.decision,
            RenderingDecision::FullRefresh { .. }
        ));
    }
}
