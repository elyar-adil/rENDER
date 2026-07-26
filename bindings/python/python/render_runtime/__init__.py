"""Python entry point for the rENDER web runtime.

The public package is intentionally separate from the native module so future
async Browser, Page, observation, policy, and trace APIs can remain stable as
the Rust implementation evolves.
"""

from ._native import (
    cascade_html_snapshot,
    computed_html_snapshot,
    parse_html_snapshot,
    query_html_snapshot,
    resolve_length_expr,
    resolve_length_expr_strict,
)

__all__ = [
    "cascade_html_snapshot",
    "computed_html_snapshot",
    "parse_html_snapshot",
    "query_html_snapshot",
    "resolve_length_expr",
    "resolve_length_expr_strict",
]
