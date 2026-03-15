from paser import *
import graphics
import layout
import res

with open("example/hao123.html", encoding="utf-8") as html_file:
    html_lines = html_file.read()

dom_tree = html.parse(html_lines)

style.bind(dom_tree, "ua/user-agent.css")

res.load(dom_tree, "example/")

layout.layout(dom_tree)

graphics.paint(dom_tree)
