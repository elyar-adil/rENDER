import graphics


def load(d, path):
    if hasattr(d, "tag") and d.tag == "img" and "src" in d.attr:
        src = d.attr["src"]
        if not src.startswith("http://") and not src.startswith("https://") and not src.startswith("//"):
            try:
                d.img = graphics.img_load(path + src)
            except Exception:
                pass
    for c in d.children:
        load(c, path)
