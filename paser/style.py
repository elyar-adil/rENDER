from paser import html, css


def get_styles(dn: html.DOMNode) -> list:
    def _get_styles(n: html.DOMNode, s: list) -> list:
        for node in n.children:
            if hasattr(node, "tag"):
                if node.tag == "style":
                    if node.children:
                        s += css.parse(node.children[0].content)
            _get_styles(node, s)
        assert isinstance(s, list)
        return s
    return _get_styles(dn, [])


def get_type(s):
    t = "tag"
    if s[0] == '.':
        t = "class"
    elif s[0] == '#':
        t = "id"
    return t


def satisfy_selector(n, s):
    if s == "*":
        return True
    t = get_type(s)
    if t == "tag":
        if n.type == "ELEM" and n.tag == s:
            return True
    else:
        if n.type == "ELEM" and t in n.attr and n.attr[t] == s[1:]:
            return True
    return False


def get_parent_satisfy(n, s):
    if satisfy_selector(n, s):
        return n
    else:
        if hasattr(n, "parent") and n.parent:
            return get_parent_satisfy(n.parent, s)
        else:
            return None


# def find_nodes(n, s):
#     """ return list of nodes satisfies selector """
#     result = []
#
#     def trav(n):
#         if satisfy_selector(n, s):
#             result.append(n)
#
#         for c in n.children:
#             trav(c)
#
#     trav(n)
#     return result


def get_prop_list(all_styles):
    arr = []
    for sty in all_styles:
        for sel in sty.selectors:
            # get selector
            s = list(filter(None, sel.replace(".", " .").replace("#", " #").split(" ")))
            for pro in sty.properties:
                arr.append([s, pro, sty.properties[pro]])
    return arr


def get_satisfy_props(n, prop_list):
    r = []
    for ps in prop_list:
        _n = n
        fit = True
        count = 0
        for p in reversed(ps[0]):
            if count == 0:
                if not satisfy_selector(_n, p):
                    fit = False
                    break
            else:
                _n = get_parent_satisfy(_n, p)
                if not _n:
                    fit = False
                    break
            count += 1
        if fit:
            r.append(ps)
    return r


def get_spec_score(t):
    if t == "id":
        return 100
    elif t == "class":
        return 10
    else:
        return 1


def is_1st_more_spec(n, p1, p2):
    s1 = 0
    for p in p1:
        s1 += get_spec_score(get_type(p))
    s2 = 0
    for p in p2:
        s2 += get_spec_score(get_type(p))

    return s1 >= s2


def get_best_prop(node, prop_list):
    d = {}
    if prop_list:
        q = ""
        for p in prop_list:
            if p[1:][0] not in d:
                d[p[1:][0]] = p[1:][1]
                q = p[0]
            else:
                if is_1st_more_spec(node, p[0], q):
                    d[p[1:][0]] = p[1:][1]
                    q = p[0]

    return d


def parse_css_file(file_name):
    with open(file_name) as file:
        return css.parse(file.read())

def inherit(n):
    if hasattr(n, "style"):
        for x in n.style:
            if n.style[x] == "inherit":
                n.style[x] = n.parent.style[x]
    for c in n.children:
        inherit(c)


def bind(dt, ua_filename):
    prop_list = get_prop_list(parse_css_file(ua_filename) + get_styles(dt))

    def _assign_style(n):
        if n.type == "ELEM":
            n.style = get_best_prop(n, get_satisfy_props(n, prop_list))
            if "style" in n.attr:
                sty = css.parse_sty(n.attr["style"])
                for s in sty:
                    n.style[s] = sty[s]

            # print(n.tag + str(n.style))
        for c in n.children:
            _assign_style(c)

    _assign_style(dt)
    inherit(dt)
