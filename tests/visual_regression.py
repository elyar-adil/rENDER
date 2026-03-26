"""Visual regression test tool for rENDER.

Usage:
    python tests/visual_regression.py --update           # save baselines
    python tests/visual_regression.py                    # compare vs baselines
    python tests/visual_regression.py --threshold 0.5   # stricter tolerance
    python tests/visual_regression.py --test floats      # single test case
"""
import sys
import os
import argparse
import base64

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# QApplication must exist before any QPixmap/QImage is created
from PyQt6.QtWidgets import QApplication
_app = QApplication.instance() or QApplication(sys.argv[:1])

from PyQt6.QtGui import QPainter, QPixmap, QImage, QColor
from PyQt6.QtCore import Qt

from engine import _pipeline
from backend.qt.painter import paint


# ---------------------------------------------------------------------------
# Test cases
# ---------------------------------------------------------------------------

TEST_CASES = [
    {
        'name': 'block_flow',
        'viewport_w': 600,
        'viewport_h': 300,
        'html': """<!doctype html><html><head><style>
            body { margin: 0; padding: 0; }
            .a { background: #4a90d9; width: 100%; height: 80px; }
            .b { background: #e74c3c; width: 60%; height: 60px; margin: 20px auto; }
            .c { background: #2ecc71; width: 80%; height: 40px; margin: 0 10%; }
        </style></head><body>
            <div class="a"></div>
            <div class="b"></div>
            <div class="c"></div>
        </body></html>""",
    },
    {
        'name': 'floats',
        'viewport_w': 600,
        'viewport_h': 300,
        'html': """<!doctype html><html><head><style>
            body { margin: 16px; font-family: Arial; font-size: 14px; }
            .left-box  { float: left;  width: 120px; height: 100px;
                         background: #3498db; margin: 0 12px 8px 0; }
            .right-box { float: right; width: 100px; height: 80px;
                         background: #e67e22; margin: 0 0 8px 12px; }
            p { color: #333; }
        </style></head><body>
            <div class="left-box"></div>
            <div class="right-box"></div>
            <p>Float layout test. Text wraps around floated elements.
               The quick brown fox jumps over the lazy dog.</p>
        </body></html>""",
    },
    {
        'name': 'inline_text',
        'viewport_w': 600,
        'viewport_h': 250,
        'html': """<!doctype html><html><head><style>
            body { margin: 16px; font-family: Arial; }
            h2   { font-size: 22px; color: #2c3e50; margin-bottom: 8px; }
            p    { font-size: 15px; line-height: 1.6; color: #555; }
            .bold   { font-weight: bold;   color: #e74c3c; }
            .italic { font-style: italic;  color: #8e44ad; }
        </style></head><body>
            <h2>Typography Test</h2>
            <p>Normal text with <span class="bold">bold span</span> and
               <span class="italic">italic span</span> inline.</p>
            <p>Second paragraph with line-height and colour inheritance.</p>
        </body></html>""",
    },
    {
        'name': 'flexbox',
        'viewport_w': 600,
        'viewport_h': 160,
        'html': """<!doctype html><html><head><style>
            body { margin: 0; }
            .container { display: flex; justify-content: space-between;
                         align-items: center; background: #ecf0f1;
                         height: 120px; padding: 0 20px; }
            .box { width: 100px; height: 80px; background: #3498db;
                   color: white; font-family: Arial; font-size: 13px; }
            .b2  { background: #2ecc71; height: 60px; }
            .b3  { background: #e74c3c; height: 90px; }
        </style></head><body>
            <div class="container">
                <div class="box">Box 1</div>
                <div class="box b2">Box 2</div>
                <div class="box b3">Box 3</div>
            </div>
        </body></html>""",
    },
    {
        'name': 'box_model',
        'viewport_w': 600,
        'viewport_h': 280,
        'html': """<!doctype html><html><head><style>
            body { margin: 0; background: #f5f5f5; }
            .outer { margin: 20px; background: #bdc3c7; padding: 15px; }
            .inner { background: #2980b9; color: white; font-family: Arial;
                     padding: 12px 20px; border: 4px solid #1a5276;
                     margin: 10px; font-size: 14px; }
        </style></head><body>
            <div class="outer">
                <div class="inner">Padding + Border + Margin</div>
                <div class="inner">Second element</div>
            </div>
        </body></html>""",
    },
    {
        'name': 'overflow_hidden',
        'viewport_w': 400,
        'viewport_h': 200,
        'html': """<!doctype html><html><head><style>
            body { margin: 20px; }
            .clip { width: 200px; height: 80px; overflow: hidden;
                    background: #ecf0f1; border: 2px solid #95a5a6; }
            .tall { background: #e74c3c; width: 180px; height: 200px; margin: 10px; }
        </style></head><body>
            <div class="clip">
                <div class="tall"></div>
            </div>
        </body></html>""",
    },
    {
        'name': 'absolute_position',
        'viewport_w': 500,
        'viewport_h': 250,
        'html': """<!doctype html><html><head><style>
            body { margin: 0; }
            .rel { position: relative; width: 400px; height: 200px;
                   background: #ecf0f1; }
            .abs-tl { position: absolute; top: 10px;    left: 10px;
                      width: 80px; height: 60px; background: #e74c3c; }
            .abs-br { position: absolute; bottom: 10px; right: 10px;
                      width: 80px; height: 60px; background: #3498db; }
        </style></head><body>
            <div class="rel">
                <div class="abs-tl"></div>
                <div class="abs-br"></div>
            </div>
        </body></html>""",
    },
    {
        'name': 'typography',
        'viewport_w': 600,
        'viewport_h': 280,
        'html': """<!doctype html><html><head><style>
            body { margin: 16px; font-family: Arial; }
            h1 { font-size: 28px; color: #2c3e50; text-decoration: underline; }
            h2 { font-size: 20px; color: #7f8c8d; }
            p  { font-size: 14px; color: #555; }
            .strike { text-decoration: line-through; color: #e74c3c; }
        </style></head><body>
            <h1>Heading One</h1>
            <h2>Heading Two</h2>
            <p>Normal paragraph text.</p>
            <p class="strike">Strikethrough text decoration.</p>
        </body></html>""",
    },
    {
        'name': 'gradient_border_radius',
        'viewport_w': 400,
        'viewport_h': 220,
        'html': """<!doctype html><html><head><style>
            body { margin: 20px; }
            .card { width: 260px; height: 140px; border-radius: 12px;
                    background-image: linear-gradient(to right, #4a90d9, #7b68ee);
                    border: 3px solid #2c3e50; }
        </style></head><body>
            <div class="card"></div>
        </body></html>""",
    },
    {
        'name': 'list_items',
        'viewport_w': 500,
        'viewport_h': 300,
        'html': """<!doctype html><html><head><style>
            body { margin: 16px; font-family: Arial; font-size: 15px; }
            ul { margin-bottom: 16px; }
            ol { color: #2c3e50; }
            li { margin-bottom: 4px; }
        </style></head><body>
            <ul>
                <li>Unordered item one</li>
                <li>Unordered item two</li>
                <li>Unordered item three</li>
            </ul>
            <ol>
                <li>Ordered first</li>
                <li>Ordered second</li>
                <li>Ordered third</li>
            </ol>
        </body></html>""",
    },
    {
        'name': 'table_basic',
        'viewport_w': 600,
        'viewport_h': 200,
        'html': """<!doctype html><html><head><style>
            body { margin: 0; }
            table { border-collapse: collapse; width: 100%; }
            td { border: 1px solid #ccc; padding: 8px; font-family: Arial; font-size: 14px; }
            .head { background: #ff6600; color: white; font-weight: bold; }
        </style></head><body>
            <table>
                <tr>
                    <td class="head">Column 1</td>
                    <td class="head">Column 2</td>
                    <td class="head">Column 3</td>
                </tr>
                <tr>
                    <td>Cell A</td><td>Cell B</td><td>Cell C</td>
                </tr>
                <tr>
                    <td>Cell D</td><td>Cell E</td><td>Cell F</td>
                </tr>
            </table>
        </body></html>""",
    },
    {
        'name': 'table_attrs',
        'viewport_w': 600,
        'viewport_h': 200,
        'html': """<!doctype html><html><head></head><body bgcolor="#f6f6ef">
            <center>
            <table border="0" cellpadding="4" cellspacing="0" width="80%" bgcolor="#ffffff">
                <tr>
                    <td bgcolor="#ff6600" width="30%"><b>Header</b></td>
                    <td bgcolor="#ff6600">Navigation</td>
                    <td bgcolor="#ff6600" align="right">Login</td>
                </tr>
                <tr>
                    <td valign="top">Left</td>
                    <td>Middle content with some text here</td>
                    <td valign="top" align="right">Right</td>
                </tr>
            </table>
            </center>
        </body></html>""",
    },
    {
        'name': 'hn_header',
        'viewport_w': 980,
        'viewport_h': 50,
        'html': """<!doctype html><html><head><style>
            body { margin: 0; font-family: Verdana, sans-serif; font-size: 10pt; }
            .pagetop { font-size: 10pt; color: #222; }
            a { color: #000; text-decoration: none; }
        </style></head><body>
            <center>
            <table id="hnmain" border="0" cellpadding="0" cellspacing="0" width="85%" bgcolor="#f6f6ef">
                <tr>
                    <td bgcolor="#ff6600">
                        <table border="0" cellpadding="0" cellspacing="0" width="100%" style="padding:2px">
                            <tr>
                                <td style="width:18px;padding-right:4px">
                                    <b>Y</b>
                                </td>
                                <td style="line-height:12pt; height:10px;">
                                    <span class="pagetop"><b>Hacker News</b>
                                    <a href="newest">new</a> | <a href="front">past</a> |
                                    <a href="comments">comments</a></span>
                                </td>
                                <td style="text-align:right;padding-right:4px;">
                                    <span class="pagetop"><a href="login">login</a></span>
                                </td>
                            </tr>
                        </table>
                    </td>
                </tr>
            </table>
            </center>
        </body></html>""",
    },
]


# ---------------------------------------------------------------------------
# Core rendering
# ---------------------------------------------------------------------------

def render_to_image(html: str, viewport_w: int, viewport_h: int) -> QImage:
    """Render HTML headlessly and return a QImage."""
    import engine as _engine_mod

    # Temporarily override the engine's viewport constants
    old_w = _engine_mod.VIEWPORT_W
    old_h = _engine_mod.VIEWPORT_H
    _engine_mod.VIEWPORT_W = viewport_w
    _engine_mod.VIEWPORT_H = viewport_h
    try:
        display_list, _page_height, _doc = _pipeline(html, base_url='')
    finally:
        _engine_mod.VIEWPORT_W = old_w
        _engine_mod.VIEWPORT_H = old_h

    pixmap = QPixmap(viewport_w, viewport_h)
    pixmap.fill(QColor(255, 255, 255))
    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    paint(display_list, painter)
    painter.end()  # must end before converting

    return pixmap.toImage()


# ---------------------------------------------------------------------------
# Image comparison
# ---------------------------------------------------------------------------

def compare_images(img1: QImage, img2: QImage) -> tuple:
    """Compare two QImages pixel by pixel.

    Returns (diff_pct: float, diff_img: QImage).
    diff_pct is percentage of pixels that differ.
    diff_img has red pixels where differences exist, white elsewhere.
    """
    w, h = img1.width(), img1.height()
    fmt = QImage.Format.Format_RGB32
    a = img1.convertToFormat(fmt)
    b = img2.convertToFormat(fmt)

    diff_img = QImage(w, h, fmt)
    diff_img.fill(QColor(255, 255, 255))

    red_rgb = QColor(220, 50, 50).rgb()
    diff_count = 0

    for y in range(h):
        for x in range(w):
            if a.pixel(x, y) != b.pixel(x, y):
                diff_count += 1
                diff_img.setPixel(x, y, red_rgb)

    total = w * h
    diff_pct = (diff_count / total * 100.0) if total > 0 else 0.0
    return diff_pct, diff_img


def _save_image(img: QImage, path: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    img.save(path, 'PNG')


def _image_to_data_uri(path: str) -> str:
    with open(path, 'rb') as f:
        return 'data:image/png;base64,' + base64.b64encode(f.read()).decode()


# ---------------------------------------------------------------------------
# Test runner
# ---------------------------------------------------------------------------

def run_tests(update: bool = False, threshold: float = 1.0,
              cases: list = None) -> int:
    """Run visual regression tests.

    Returns 0 if all pass, 1 if any fail.
    """
    baselines_dir = os.path.join(os.path.dirname(__file__), 'baselines')
    report_path   = os.path.join(os.path.dirname(__file__), 'visual_report.html')

    if cases is None:
        cases = TEST_CASES

    results = []
    passed = failed = 0

    print(f'\n{"Updating baselines" if update else "Running visual regression tests"}'
          f' ({len(cases)} case{"s" if len(cases) != 1 else ""})\n')

    for tc in cases:
        name       = tc['name']
        html       = tc['html']
        vw         = tc['viewport_w']
        vh         = tc['viewport_h']
        baseline_p = os.path.join(baselines_dir, f'{name}.png')
        current_p  = os.path.join(baselines_dir, f'{name}_current.png')
        diff_p     = os.path.join(baselines_dir, f'{name}_diff.png')

        label = f'  {name:<30}'
        print(label, end='', flush=True)

        try:
            current_img = render_to_image(html, vw, vh)
        except Exception as exc:
            print(f'ERROR  {exc}')
            results.append({'name': name, 'status': 'error',
                             'msg': str(exc)})
            failed += 1
            continue

        if update:
            _save_image(current_img, baseline_p)
            print('SAVED  baseline')
            results.append({'name': name, 'status': 'baseline',
                             'baseline': baseline_p})
            continue

        if not os.path.exists(baseline_p):
            print('SKIP   no baseline — run with --update first')
            results.append({'name': name, 'status': 'missing'})
            continue

        baseline_img = QImage(baseline_p)
        if (baseline_img.width() != current_img.width() or
                baseline_img.height() != current_img.height()):
            bw, bh = baseline_img.width(), baseline_img.height()
            cw, ch = current_img.width(), current_img.height()
            print(f'FAIL   size mismatch baseline={bw}x{bh} current={cw}x{ch}')
            _save_image(current_img, current_p)
            results.append({'name': name, 'status': 'fail',
                             'diff_pct': 100.0,
                             'msg': f'size mismatch {bw}x{bh} vs {cw}x{ch}',
                             'baseline': baseline_p,
                             'current': current_p,
                             'diff': None})
            failed += 1
            continue

        diff_pct, diff_img = compare_images(baseline_img, current_img)
        ok = diff_pct <= threshold

        _save_image(current_img, current_p)
        _save_image(diff_img, diff_p)

        status = 'pass' if ok else 'fail'
        mark   = 'PASS ' if ok else 'FAIL '
        print(f'{mark}  {diff_pct:.2f}% diff')

        results.append({'name': name, 'status': status,
                        'diff_pct': diff_pct,
                        'baseline': baseline_p,
                        'current': current_p,
                        'diff': diff_p})
        if ok:
            passed += 1
        else:
            failed += 1

    if not update:
        _write_report(results, report_path, threshold)
        total = passed + failed
        verdict = 'All passed' if failed == 0 else f'{failed} failed'
        print(f'\n{verdict}  ({passed}/{total} passed)')
        print(f'Report: {report_path}')

    return 0 if failed == 0 else 1


# ---------------------------------------------------------------------------
# HTML report
# ---------------------------------------------------------------------------

_STATUS_COLOR = {
    'pass':     '#27ae60',
    'fail':     '#e74c3c',
    'missing':  '#e67e22',
    'baseline': '#3498db',
    'error':    '#8e44ad',
}


def _write_report(results: list, path: str, threshold: float) -> None:
    rows = []
    for r in results:
        status = r['status']
        color  = _STATUS_COLOR.get(status, '#95a5a6')
        badge  = (f'<span style="background:{color};color:white;padding:2px 8px;'
                  f'border-radius:4px;font-size:12px">{status.upper()}</span>')

        diff_cell = ''
        if 'diff_pct' in r:
            pct = r['diff_pct']
            diff_cell = f'{pct:.2f}%'
        msg_cell = r.get('msg', '')

        img_cells = ''
        if (status in ('pass', 'fail') and
                r.get('baseline') and os.path.exists(r['baseline']) and
                r.get('current') and os.path.exists(r['current'])):
            b64_b = _image_to_data_uri(r['baseline'])
            b64_c = _image_to_data_uri(r['current'])
            b64_d = (_image_to_data_uri(r['diff'])
                     if r.get('diff') and os.path.exists(r['diff']) else '')
            diff_img_tag = (f'<img src="{b64_d}" style="max-width:200px">'
                            if b64_d else '—')
            img_cells = (
                f'<td><img src="{b64_b}" style="max-width:200px"><br>'
                f'<small>baseline</small></td>'
                f'<td><img src="{b64_c}" style="max-width:200px"><br>'
                f'<small>current</small></td>'
                f'<td>{diff_img_tag}<br><small>diff</small></td>'
            )
        elif status == 'baseline' and r.get('baseline') and os.path.exists(r['baseline']):
            b64_b = _image_to_data_uri(r['baseline'])
            img_cells = (f'<td colspan="3"><img src="{b64_b}" style="max-width:200px">'
                         f'<br><small>saved baseline</small></td>')
        else:
            img_cells = f'<td colspan="3"><em>{msg_cell or status}</em></td>'

        rows.append(
            f'<tr><td><strong>{r["name"]}</strong></td>'
            f'<td style="text-align:center">{badge}</td>'
            f'{img_cells}'
            f'<td style="text-align:right">{diff_cell}</td></tr>'
        )

    total  = len(results)
    n_pass = sum(1 for r in results if r['status'] == 'pass')
    n_fail = sum(1 for r in results if r['status'] == 'fail')
    summary = f'{n_pass}/{total} passed, {n_fail} failed  (threshold: {threshold}%)'

    html = f"""<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>rENDER Visual Regression</title>
<style>
  body  {{ font-family: Arial, sans-serif; margin: 24px; background: #f9f9f9; color: #333; }}
  h1   {{ color: #2c3e50; margin-bottom: 4px; }}
  .summary {{ color: #555; margin-bottom: 20px; }}
  table {{ border-collapse: collapse; width: 100%; background: white;
           box-shadow: 0 1px 4px rgba(0,0,0,.1); }}
  th, td {{ border: 1px solid #e0e0e0; padding: 8px 12px; vertical-align: top; }}
  th   {{ background: #ecf0f1; font-weight: 600; }}
  tr:hover {{ background: #f5faff; }}
  img  {{ display: block; border: 1px solid #ccc; margin-bottom: 2px; }}
  small {{ color: #888; font-size: 11px; }}
</style>
</head><body>
<h1>rENDER Visual Regression Report</h1>
<p class="summary">{summary}</p>
<table>
<thead>
  <tr>
    <th style="width:160px">Test</th>
    <th style="width:80px">Status</th>
    <th>Baseline</th><th>Current</th><th>Diff</th>
    <th style="width:70px">Diff %</th>
  </tr>
</thead>
<tbody>
{''.join(rows)}
</tbody>
</table>
</body></html>"""

    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w', encoding='utf-8') as f:
        f.write(html)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description='rENDER visual regression tests')
    parser.add_argument('--update', action='store_true',
                        help='Re-render and save baselines')
    parser.add_argument('--threshold', type=float, default=1.0,
                        metavar='PCT',
                        help='Max allowed diff %% (default 1.0)')
    parser.add_argument('--test', metavar='NAME',
                        help='Run only the named test case')
    args = parser.parse_args()

    cases = TEST_CASES
    if args.test:
        cases = [tc for tc in TEST_CASES if tc['name'] == args.test]
        if not cases:
            names = ', '.join(tc['name'] for tc in TEST_CASES)
            print(f'Unknown test "{args.test}". Available: {names}')
            sys.exit(2)

    sys.exit(run_tests(update=args.update, threshold=args.threshold,
                       cases=cases))


if __name__ == '__main__':
    main()
