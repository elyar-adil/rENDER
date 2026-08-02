"""Responsive image source selection for ``picture`` and ``srcset``."""

from __future__ import annotations

import re

from html.dom import Element


def select_responsive_images(document, viewport_width: int) -> None:
    """Select common width/density candidates for a 1x viewport."""
    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element) and node.tag == 'picture':
            _select_picture(node, viewport_width)
        elif isinstance(node, Element) and node.tag == 'img':
            parent = getattr(node, 'parent', None)
            if not isinstance(parent, Element) or parent.tag != 'picture':
                _select_img_srcset(node, viewport_width)
        stack.extend(reversed(getattr(node, 'children', [])))


def _select_picture(picture: Element, viewport_width: int) -> None:
    image = next(
        (child for child in picture.children
         if isinstance(child, Element) and child.tag == 'img'),
        None,
    )
    if image is None:
        return
    _remember_fallback(image)

    for child in picture.children:
        if not isinstance(child, Element) or child.tag != 'source':
            continue
        srcset = child.attributes.get('srcset', '').strip()
        media = child.attributes.get('media', '').strip()
        if srcset and _media_matches(media, viewport_width):
            selected = _select_srcset_candidate(srcset, viewport_width)
            if selected:
                image.attributes['src'] = selected
                return

    if image.attributes.get('srcset'):
        _select_img_srcset(image, viewport_width)
    else:
        image.attributes['src'] = getattr(image, '_responsive_fallback_src', '')


def _select_img_srcset(image: Element, viewport_width: int) -> None:
    _remember_fallback(image)
    selected = _select_srcset_candidate(
        image.attributes.get('srcset', ''),
        viewport_width,
    )
    image.attributes['src'] = selected or getattr(image, '_responsive_fallback_src', '')


def _remember_fallback(image: Element) -> None:
    if not hasattr(image, '_responsive_fallback_src'):
        image._responsive_fallback_src = image.attributes.get('src', '')


def _select_srcset_candidate(srcset: str, viewport_width: int) -> str | None:
    candidates: list[tuple[str, float, str]] = []
    for raw_candidate in srcset.split(','):
        parts = raw_candidate.strip().split()
        if not parts:
            continue
        url = parts[0]
        descriptor = parts[1].lower() if len(parts) > 1 else '1x'
        try:
            if descriptor.endswith('w'):
                candidates.append((url, float(descriptor[:-1]), 'w'))
            elif descriptor.endswith('x'):
                candidates.append((url, float(descriptor[:-1]), 'x'))
            else:
                candidates.append((url, 1.0, 'x'))
        except ValueError:
            continue
    if not candidates:
        return None

    kind = 'w' if any(candidate[2] == 'w' for candidate in candidates) else 'x'
    matching = sorted(
        (candidate for candidate in candidates if candidate[2] == kind),
        key=lambda candidate: candidate[1],
    )
    target = float(viewport_width) if kind == 'w' else 1.0
    for url, value, _kind in matching:
        if value >= target:
            return url
    return matching[-1][0]


def _media_matches(media: str, viewport_width: int) -> bool:
    if not media:
        return True
    min_match = re.search(r'min-width\s*:\s*([\d.]+)px', media, re.IGNORECASE)
    if min_match and viewport_width < float(min_match.group(1)):
        return False
    max_match = re.search(r'max-width\s*:\s*([\d.]+)px', media, re.IGNORECASE)
    if max_match and viewport_width > float(max_match.group(1)):
        return False
    return True
