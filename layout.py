import graphics

viewport_width = graphics.viewport_width


class Box:
    def __init__(self, x, y, width, height):
        self.x = x
        self.y = y
        self.cy = 0
        self.width = width
        self.height = height


def get_layout_width(n):
    if n.type == "ROOT":
        return viewport_width
    elif n.type == "ELEM":
        if "width" in n.style:
            if n.style["width"] == "inherit":
                width = get_layout_width(n.parent)
            else:
                if n.style["width"][-1] == '%':
                    parent_width = get_layout_width(n.parent)
                    width = int(parent_width * (int(n.style["width"][0:-1]) / 100))
                else:
                    width = px2pixel(n.style["width"])
    elif n.type == "WORD":
        return graphics.get_text_size(n)[0]
    return int(width)


def get_layout_height(n):
    if n.type == "ELEM" and "height" in n.style:
        return int(n.style["height"][:-2])
    elif n.type == "WORD":
        return graphics.get_text_size(n)[1]
    else:
        height = 0
        last_word = None
        for c in n.children:
            if get_layout_float(c) == "none":
                height += get_layout_height(c)
            elif c.type == "WORD":
                last_word = c
        if last_word:
            height = n.parent.box.y - last_word.box.y + last_word.box.height
    return height


class Margin:
    def __str__(self):
        return str((self.t, self.r, self.b, self.l))

    def __init__(self, t, r, b, l):
        self.r = r
        self.l = l
        self.t = t
        self.b = b


def px2pixel(s):
    s = s.strip()
    if s == "0" or s == "auto" or s == "":
        return 0
    if s.endswith("px"):
        try:
            return int(float(s[:-2]))
        except ValueError:
            return 0
    if s.endswith("em") or s.endswith("rem") or s.endswith("vw") or s.endswith("vh"):
        try:
            return int(float(s[:-2]) * 16)
        except ValueError:
            return 0
    try:
        return int(float(s))
    except ValueError:
        return 0


def get_margin(n):
    if not hasattr(n, "style"):
        return
    n.margin = Margin(0, 0, 0, 0)
    if "margin" in n.style:
        l = n.style["margin"].split(" ")
        if len(l) == 1:
            n.margin = Margin(px2pixel(l[0]), px2pixel(l[0]), px2pixel(l[0]), px2pixel(l[0]))
        elif len(l) == 2:
            n.margin = Margin(px2pixel(l[0]), px2pixel(l[1]), px2pixel(l[0]), px2pixel(l[1]))
        elif len(l) == 4:
            n.margin = Margin(px2pixel(l[0]), px2pixel(l[1]), px2pixel(l[2]), px2pixel(l[3]))
    if "margin-left" in n.style:
        n.margin.l = px2pixel(n.style["margin-left"])
    if "margin-right" in n.style:
        n.margin.r = px2pixel(n.style["margin-right"])
    if "margin-top" in n.style:
        n.margin.t = px2pixel(n.style["margin-top"])
    if "margin-bottom" in n.style:
        n.margin.b = px2pixel(n.style["margin-bottom"])


def get_layout_float(n):
    f = "none"
    if n.type == "ELEM":
        if "float" in n.style:
            f = n.style["float"]
    elif n.type == "WORD":
        f = "left"
    return f


class WordRender:
    def __init__(self, word, word_spacing):
        self.word = word
        self.type = "WORD"
        self.style = {}
        self.word_spacing = word_spacing
        self.children = []


boxes = []


def layout(t):
    # init root box width
    def _layout_children(d):
        d.cy = 0
        for n in d.children:
            _layout(n)

    def _layout(d):
        if d.type == "TEXT":
            if d.parent.style["display"] != "none":
                words = d.content.split(" ")
                words = [x for x in words if x != '']
                d.children = []
                for w in words:
                    r = WordRender(w, int(d.parent.style["word-spacing"][:-2]))
                    r.font = (d.parent.style["font-family"], int(d.parent.style["font-size"][:-2]))
                    r.color = d.parent.style["color"]
                    r.decoration = d.parent.style["text-decoration"]
                    r.parent = d.parent
                    d.children.append(r)
                _layout_children(d)
            return
        else:
            d.float_children = []
        if d.type == "ROOT":
            x = 0
            y = 0
            w = viewport_width
            d.box = Box(x, y, w, None)
            d.cy = 0
            _layout_children(d)
            d.box.h = get_layout_height(d)
        else:
            f = get_layout_float(d)
            w = get_layout_width(d)
            get_margin(d)
            y = d.parent.box.y + d.parent.cy + d.margin.t
            if d.type == "WORD" and hasattr(d.parent, "wy"):
                y = d.parent.wy
            h = get_layout_height(d)
            if hasattr(d, "tag") and d.tag == "img" and hasattr(d, "img"):
                if "height" not in d.style and "width" not in d.style:
                    w = d.img.size[0]
                    h = d.img.size[1]
                else:
                    if "height" not in d.style:
                        h = int(w / d.img.size[0] * d.img.size[1])
                    elif "width" not in d.style:
                        w = int(h / d.img.size[1] * d.img.size[0])
                d.img = graphics.img_resize(d.img, (w, h))
            if f != "none":
                if d.type == "WORD" and not hasattr(d.parent, "wy"):
                    d.parent.wy = 0
                # float left
                if f == "left":
                    x = d.parent.box.x + d.margin.l
                    i = 0
                    while i < len(boxes):
                        b = boxes[i]
                        i += 1
                        if hasattr(b, "isword") and d.type != "WORD":
                            continue
                        if b.y - b.margin.t <= y < b.y + b.height + b.margin.b and x + w > b.x - d.margin.l and x <= b.x + b.width + d.margin.r:
                            x = b.x + b.width + b.margin.r + d.margin.r
                            # TODO:support padding here
                            if x + w > d.parent.box.x + d.parent.box.width: # - d.padding.r:
                                y = b.y + b.height + d.margin.t
                                x = d.parent.box.x + d.margin.l
                                i = 0
                    if d.type == "WORD":
                        d.parent.wy = y
                    d.box = Box(x, y, w, h)
                    d.box.margin = d.margin
                    if d.type == "WORD":
                        d.box.isword = True
                # float right
                elif f == "right":
                    x = d.parent.box.x + d.parent.box.width - d.margin.r - w
                    i = 0
                    while i < len(boxes):
                        b = boxes[i]
                        i += 1
                        if hasattr(b, "isword") and d.type != "WORD":
                            continue
                        if b.y - b.margin.t <= y < b.y + b.height + b.margin.b and x + w > b.x - d.margin.l and x <= b.x + b.width + d.margin.r:
                            x = b.x - w - d.margin.l - b.margin.r
                            # TODO:support padding here
                            if x < d.parent.box.x:#  + d.parent.padding.l
                                y = b.y + b.height + d.margin.t
                                x = d.parent.box.x + d.parent.box.width - d.margin.r - w
                                i = 0
                    if d.type == "WORD":
                        d.parent.wy = y
                    d.box = Box(x, y, w, h)
                    d.box.margin = d.margin
                _layout_children(d)
                boxes.append(d.box)
            else:
                x = d.parent.box.x + d.margin.l
                d.box = Box(x, y, w, None)
                d.box.margin = d.margin
                _layout_children(d)
                d.box.height = get_layout_height(d)
                d.parent.cy += d.box.height + d.margin.t + d.margin.b
                boxes.append(d.box)
                # if "margin" in d.style and d.style["margin"] == "auto":
                #     ml = "margin_left" in d.style
                #     mr = "margin_right" in d.style
                #     if not ml and not mr:
                #         d.style["margin-left"] = d.style["margin-right"] = (d.parent.box.width - w) / 2
                #     elif mr and not ml:
                #         d.style["margin-left"] = (d.parent.box.width - w)
                #     else:
                #         d.style["margin-right"] = (d.parent.box.width - w)
                #
                #     w = d.parent.box.width

    _layout(t)

