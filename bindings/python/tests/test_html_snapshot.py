from render_runtime import parse_html_snapshot


def descendants(node, local_name):
    found = []
    if node.get("local_name") == local_name:
        found.append(node)
    for child in node["children"]:
        found.extend(descendants(child, local_name))
    return found


def text_content(node):
    if node["type"] == "text":
        return node["data"]
    return "".join(text_content(child) for child in node["children"])


def test_snapshot_exposes_stable_ids_structure_and_errors():
    snapshot = parse_html_snapshot("<p id=first id=second>Hello</p>")
    assert snapshot["quirks_mode"] == "quirks"
    assert {error["code"] for error in snapshot["errors"]} >= {
        "missing-doctype",
        "duplicate-attribute",
    }

    document = snapshot["document"]
    paragraph = descendants(document, "p")[0]
    assert paragraph["attributes"] == {"id": "first"}
    assert text_content(paragraph) == "Hello"

    ids = []

    def collect(node):
        ids.append(node["id"])
        for child in node["children"]:
            collect(child)

    collect(document)
    assert len(ids) == len(set(ids))
    assert ids[0] == 0


def test_snapshot_matches_edge_tree_construction_contract():
    snapshot = parse_html_snapshot(
        "<!doctype html><html lang=en><head><title>T&amp;</title></head>"
        "<body id=first><body id=second class=page>"
        "<p>one<div>two</div><ul><li>a<li>b</ul>"
        "<div><table>outside<tr><td>A<td>B</table></div>"
        "<textarea>\nA&amp;<b></textarea>"
    )
    document = snapshot["document"]
    assert snapshot["quirks_mode"] == "no-quirks"
    html = descendants(document, "html")[0]
    body = descendants(html, "body")[0]
    assert html["attributes"] == {"lang": "en"}
    assert body["attributes"] == {"id": "first", "class": "page"}
    assert [text_content(node) for node in descendants(body, "li")] == ["a", "b"]

    table = descendants(body, "table")[0]
    assert [child.get("local_name") for child in table["children"] if child["type"] == "element"] == [
        "tbody"
    ]
    assert [text_content(node) for node in descendants(table, "td")] == ["A", "B"]
    assert text_content(descendants(body, "textarea")[0]) == "A&<b>"
