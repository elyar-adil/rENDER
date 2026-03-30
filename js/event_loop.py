"""Minimal event-loop core for script/timer/network task + microtask coordination.

The runtime is still single-threaded/synchronous, but this module centralizes
queueing semantics so Promise reactions are no longer drained inline from inside
Promise settlement.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from typing import Callable

Task = Callable[[], None]


@dataclass
class EventLoop:
    """Cooperative event loop with macrotasks and microtasks."""

    task_queues: dict[str, deque[Task]] = field(default_factory=lambda: {
        'script': deque(),
        'timer': deque(),
        'network': deque(),
        'user-interaction': deque(),
    })
    microtasks: deque[Task] = field(default_factory=deque)
    max_steps: int = 10_000

    def enqueue_task(self, queue: str, fn: Task) -> None:
        self.task_queues.setdefault(queue, deque()).append(fn)

    def enqueue_microtask(self, fn: Task) -> None:
        self.microtasks.append(fn)

    def perform_microtask_checkpoint(self) -> None:
        steps = 0
        while self.microtasks and steps < self.max_steps:
            steps += 1
            task = self.microtasks.popleft()
            try:
                task()
            except Exception:
                pass

    def run_next_task(self) -> bool:
        for queue in ('script', 'timer', 'network', 'user-interaction'):
            q = self.task_queues.get(queue)
            if not q:
                continue
            task = q.popleft()
            try:
                task()
            except Exception:
                pass
            self.perform_microtask_checkpoint()
            return True
        return False

    def run_until_idle(self) -> None:
        steps = 0
        while steps < self.max_steps and self.run_next_task():
            steps += 1
        # Always perform a final checkpoint for callers that only queued microtasks.
        self.perform_microtask_checkpoint()

    def set_timeout(self, callback: Callable, *args, _interp=None) -> int:
        def _task():
            if _interp is not None:
                _interp._call_value(callback, list(args))
            elif callable(callback):
                callback(*args)

        self.enqueue_task('timer', _task)
        return 1


_DEFAULT_EVENT_LOOP = EventLoop()


def get_event_loop() -> EventLoop:
    return _DEFAULT_EVENT_LOOP


def reset_event_loop() -> EventLoop:
    global _DEFAULT_EVENT_LOOP
    _DEFAULT_EVENT_LOOP = EventLoop()
    return _DEFAULT_EVENT_LOOP
