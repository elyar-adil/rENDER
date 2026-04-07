"""Shared CSS parsing utilities."""


def split_paren_aware(text: str, sep: str = ',') -> list[str]:
    """Split *text* by *sep* while ignoring separators inside parentheses.

    Examples::

        split_paren_aware('a, b, c')             == ['a', ' b', ' c']
        split_paren_aware('rgb(1,2,3), blue')     == ['rgb(1,2,3)', ' blue']
        split_paren_aware('a and b', sep=' and ') # Note: only single-char sep supported
    """
    result: list[str] = []
    depth: int = 0
    current: list[str] = []
    sep_len: int = len(sep)
    i: int = 0
    n: int = len(text)
    while i < n:
        ch: str = text[i]
        if ch == '(':
            depth += 1
            current.append(ch)
            i += 1
        elif ch == ')':
            depth -= 1
            current.append(ch)
            i += 1
        elif depth == 0 and text[i:i + sep_len] == sep:
            result.append(''.join(current))
            current: list[str] = []
            i += sep_len
        else:
            current.append(ch)
            i += 1
    if current:
        result.append(''.join(current))
    return result
