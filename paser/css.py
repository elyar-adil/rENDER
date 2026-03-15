class Style:
    def __init__(self, selectors, properties):
        self.selectors = selectors
        self.properties = properties


def parse_sty(sty_str):
    sty_str = sty_str.replace("\n", "").replace(";", "\n")
    properties = {}
    for line in sty_str.splitlines():
        if line:
            t = [x.strip(' \t\n\r') for x in line.split(":")]
            if len(t) >= 2 and t[0] and t[1]:
                properties[t[0]] = t[1]
    return properties


def parse(css_lines):
    css_lines = css_lines.replace("\n", "")
    pos = 0
    styles = []
    while pos < len(css_lines):
        sel_end = css_lines.find("{", pos)
        if sel_end == -1:
            break
        sel_str = css_lines[pos:sel_end]
        pos = sel_end + 1
        sty_end = css_lines.find("}", pos)
        sty_str = css_lines[pos:sty_end]
        pos = sty_end + 1

        selectors = [x.strip(' \t\n\r') for x in sel_str.split(",")]
        styles.append(Style(selectors, parse_sty(sty_str)))
    return styles
