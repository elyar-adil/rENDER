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
    render_callback: Task | None = None
    render_requested: bool = False
    animation_frame_callbacks: deque[tuple[int, Callable[[float], None]]] = field(default_factory=deque)
    _next_animation_frame_id: int = 1

    def enqueue_task(self, queue: str, fn: Task) -> None:
        self.task_queues.setdefault(queue, deque()).append(fn)

    def enqueue_microtask(self, fn: Task) -> None:
        self.microtasks.append(fn)

    def set_render_callback(self, fn: Task | None) -> None:
        self.render_callback = fn

    def request_render(self) -> None:
        self.render_requested = True

    def request_animation_frame(self, fn: Callable[[float], None]) -> int:
        request_id = self._next_animation_frame_id
        self._next_animation_frame_id += 1
        self.animation_frame_callbacks.append((request_id, fn))
        self.request_render()
        return request_id

    def cancel_animation_frame(self, request_id: int) -> None:
        self.animation_frame_callbacks = deque(
            item for item in self.animation_frame_callbacks
            if item[0] != request_id
        )

    def perform_microtask_checkpoint(self) -> None:
        steps = 0
        while self.microtasks and steps < self.max_steps:
            steps += 1
            task = self.microtasks.popleft()
            try:
                task()
            except Exception:
                pass

    def perform_rendering_opportunity(self) -> None:
        if not self.render_requested:
            return
        self.render_requested = False
        callbacks = list(self.animation_frame_callbacks)
        self.animation_frame_callbacks.clear()
        for _, callback in callbacks:
            try:
                callback(0.0)
            except Exception:
                pass
        if callbacks:
            self.perform_microtask_checkpoint()
        if self.render_callback is None:
            return
        try:
            self.render_callback()
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
            self.perform_rendering_opportunity()
            return True
        return False

    def run_until_idle(self) -> None:
        steps = 0
        while steps < self.max_steps and self.run_next_task():
            steps += 1
        # Always perform a final checkpoint for callers that only queued microtasks.
        self.perform_microtask_checkpoint()
        self.perform_rendering_opportunity()

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
