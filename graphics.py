from tkinter import *
from PIL import Image, ImageTk

viewport_width = 980
viewport_height = 600
canvas = Canvas(Tk(), width=viewport_width, height=viewport_height, background="white")

canvas.pack()


def get_text_size(d):
    bounds = canvas.bbox(canvas.create_text(-1000, -1000, text=d.word, font=d.font))
    width = bounds[2] - bounds[0]
    height = bounds[3] - bounds[1]
    return width + d.word_spacing, height


def img_load(file_name):
    return Image.open(file_name)


def img_resize(img, size):
    return img.resize((int(size[0]), int(size[1])))


def paint(d):

    def _paint_background(d):
        if d.type == "ELEM":
            if hasattr(d, "box"):
                canvas.create_rectangle(d.box.x, d.box.y, d.box.x + d.box.width, d.box.y + d.box.height,
                                        width=0, fill=d.style["background-color"])

        for c in d.children:
            _paint_background(c)

    def _paint_words(d):
        if d.type == "WORD":
            canvas.create_text(d.box.x, d.box.y, text=d.word, fill=d.color, font=d.font, anchor=NW)
            if d.decoration != "none":
                if d.decoration == "underline":
                    dy = d.box.height
                elif d.decoration == "line-through":
                    dy = d.box.height/2
                elif d.decoration == "overline":
                    dy = 0
                canvas.create_line(d.box.x, d.box.y + dy, d.box.x + d.box.width, d.box.y + dy, fill=d.color)

        for c in d.children:
            _paint_words(c)

    def _paint_imgs(d):
        if hasattr(d, "tag") and d.tag == "img":
            if hasattr(d, "img"):
                d.imgtk = ImageTk.PhotoImage(d.img) # to prevent the image garbage collected.
                canvas.create_image(d.box.x, d.box.y, image=d.imgtk, anchor='nw')
                pass
        for c in d.children:
            _paint_imgs(c)

    _paint_background(d)
    _paint_imgs(d)
    _paint_words(d)

    mainloop()



