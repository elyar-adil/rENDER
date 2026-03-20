# JavaScript 最小可行子集分析

> 目标：实现恰好足够渲染 qq.com / hao123 类门户网站内容的 JS 子集

---

## 关键洞察：别自己写 JS 引擎

**rENDER 已经依赖 PyQt6，而 PyQt6 内置了完整的 JS 引擎：`QJSEngine`（来自 `PyQt6.QtQml`）。**

```python
from PyQt6.QtQml import QJSEngine

engine = QJSEngine()
result = engine.evaluate("1 + 1")       # → 2
engine.evaluate("function f(x){return x*2}")
engine.evaluate("f(21)")                # → 42
```

QJSEngine 底层是 V8（通过 Qt 的 JavaScriptCore），ES2020+ 语法全支持。
**我们只需要实现 DOM bindings，JS 语言层完全免费。**

---

## hao123/qq.com 实际使用的 JS 模式（从源码分析）

通过分析 `example/hao123.html`（同类门户），实际使用的 JS 模式如下：

```javascript
// 1. IIFE 模块模式（几乎所有代码块）
(function(win) { ... })(window);

// 2. 命名空间对象
window.HAO = window.HAO || {};

// 3. querySelector 替代函数
document.querySelectorAll('.' + className)[0]

// 4. innerHTML 注入 HTML 字符串（关键！大量内容靠这个注入）
element.innerHTML = '<div class="news-item">...</div>';

// 5. XHR 请求（内容加载核心）
var xhr = new XMLHttpRequest();
xhr.open('GET', url + '?' + params);
xhr.onreadystatechange = function() {
    if (xhr.readyState === 4 && xhr.status === 200) {
        var res = JSON.parse(xhr.response);
        callback(res);
    }
};
xhr.send();

// 6. 动态 script 插入（加载更多模块）
var hm = document.createElement("script");
hm.src = "https://cdn.example.com/module.js";
document.getElementsByTagName("script")[0].parentNode.insertBefore(hm, s);

// 7. 事件绑定（新旧两种写法）
node.addEventListener('click', fn);
node.attachEvent('onclick', fn);  // 兼容 IE（可以忽略）

// 8. style 直接操作
element.style.display = 'none';
element.style.backgroundImage = 'url(...)';
node.style.top = '-100%';

// 9. JSON 数据解析（数据嵌入 HTML 的 data island 模式）
var data = JSON.parse(document.querySelector('.head-links').innerText);

// 10. history API（SPA 路由）
history.pushState(null, null, newUrl);
```

---

## 按渲染收益排序的实现清单

### 🔴 Tier 1：实现后内容可见度从 10% → 60%

这些 API 是内容注入的核心，不实现则页面基本为空。

#### DOM 读写（必须实现）

| API | 用途 | 实现难度 |
|-----|------|----------|
| `element.innerHTML = html` | **最关键**：将 HTML 字符串解析为 DOM 并插入 | 中（调用已有 HTML parser） |
| `element.innerText` / `.textContent` | 读写纯文本 | 低 |
| `element.style.xxx = value` | 动态改变样式 | 中（需触发重新 layout） |
| `element.className` | 读写 class | 低 |
| `element.href` / `.src` / `.value` | 常见属性 | 低 |
| `document.querySelector(sel)` | 查找单个元素 | 低（已有 selector 引擎） |
| `document.querySelectorAll(sel)` | 查找多个元素 | 低 |
| `document.getElementById(id)` | 按 id 查找 | 低 |
| `document.getElementsByClassName(c)` | 按 class 查找 | 低 |
| `document.getElementsByTagName(t)` | 按标签查找 | 低 |
| `element.setAttribute(k, v)` | 设置属性 | 低 |
| `element.getAttribute(k)` | 读取属性 | 低 |

#### DOM 结构修改

| API | 用途 | 实现难度 |
|-----|------|----------|
| `document.createElement(tag)` | 创建元素 | 低 |
| `document.createTextNode(text)` | 创建文本节点 | 低 |
| `parent.appendChild(child)` | 追加子节点 | 低 |
| `parent.insertBefore(new, ref)` | 插入到前面 | 低 |
| `parent.removeChild(child)` | 删除子节点 | 低 |
| `element.parentNode` | 访问父节点 | 低 |
| `element.children` | 子元素列表 | 低 |

#### Window 基础对象

| API | 用途 | 实现难度 |
|-----|------|----------|
| `window.location.href` | 读当前 URL | 低（只读即可） |
| `window.location.search` | URL 查询参数 | 低 |
| `document.URL` | 同上 | 低 |
| `console.log/error/warn` | 调试（stub 即可） | 极低 |
| `setTimeout(fn, ms)` | 延时执行（内容动画用） | 中（需集成到 Qt 事件循环） |

---

### 🟠 Tier 2：实现后内容可见度从 60% → 80%

#### 网络请求（内容动态加载）

| API | 用途 | 实现难度 |
|-----|------|----------|
| `XMLHttpRequest` | qq.com 主要用 XHR 加载数据 | 中（Python urllib 桥接） |
| `JSON.parse` / `JSON.stringify` | 解析 API 返回数据 | 极低（QJSEngine 内置） |
| `fetch(url)` → Promise | 现代替代 XHR | 高（需要 Promise 支持） |

注：`JSON` 是 QJSEngine 内置的，不需要实现。`fetch` 可以用 XHR 先代替。

#### 动态 Script 加载

```javascript
// 这个模式用于加载内容模块，非常常见
var script = document.createElement('script');
script.src = url;
document.head.appendChild(script);
```

实现方式：监听 `script` 元素的 `src` 属性设置，触发下载并执行。

| API | 用途 | 实现难度 |
|-----|------|----------|
| `<script src>` 动态注入执行 | 加载内容模块 | 中 |

#### classList API

| API | 用途 | 实现难度 |
|-----|------|----------|
| `element.classList.add(c)` | 添加 class | 低 |
| `element.classList.remove(c)` | 删除 class | 低 |
| `element.classList.toggle(c)` | 切换 class（tab 切换） | 低 |
| `element.classList.contains(c)` | 判断 | 低 |

---

### 🟡 Tier 3：实现后交互性从 0% → 有限交互

#### 事件系统

| API | 用途 | 实现难度 |
|-----|------|----------|
| `element.addEventListener(ev, fn)` | 绑定事件 | 高（需要 Qt 信号桥接） |
| `event.preventDefault()` | 阻止默认行为 | 中 |
| `event.stopPropagation()` | 停止冒泡 | 中 |
| `event.target` | 事件目标元素 | 中 |
| `DOMContentLoaded` 事件 | 页面加载完成触发 | 中 |
| `window.onload` | 全部加载完成 | 中 |

#### dataset

```javascript
element.dataset.id     // → element.getAttribute('data-id')
```

| API | 实现难度 |
|-----|----------|
| `element.dataset.xxx` | 低（映射到 data-* 属性） |

---

### 🟢 Tier 4：可以不实现（或 stub 掉）

以下 API 在门户网站中用于分析/追踪，stub 成空函数不影响内容渲染：

```javascript
// Analytics & tracking（stub 成 no-op）
window._hmt = [];
window.gtag = function(){};
window.ga = function(){};
window.__spyHead.init({...});  // 性能监控

// 高级布局 API（用不到）
element.getBoundingClientRect()   // 可返回 {top:0, left:0, width:0, height:0}
window.IntersectionObserver       // stub：立即触发 callback
window.MutationObserver          // stub：忽略

// Web Storage（可 stub 成内存存储）
localStorage.getItem/setItem
sessionStorage.getItem/setItem

// History API（可 stub）
history.pushState
history.replaceState
```

---

## 实现架构建议

### 推荐方案：QJSEngine + DOM Bindings

```
QJSEngine（语言层，PyQt6 内置，免费）
    ↕ Python <-> JS 桥接（QJSEngine.newQObject）
DOM Bindings（Python 实现，约 500 行代码）
    ↓ 调用
html/dom.py（已有的 DOM 树）
    ↓ 修改后触发
重新 layout + render
```

#### 实现步骤

**Step 1：桥接层（`js/engine.py`，约 100 行）**
```python
from PyQt6.QtQml import QJSEngine

class JSEngine:
    def __init__(self, document, viewport_size, network_fn):
        self.engine = QJSEngine()
        self.dom_binding = DOMBinding(self.engine, document)
        self.dom_binding.setup()   # 注入 document, window, console

    def run_scripts(self):
        """执行页面中所有 <script> 标签"""
        for script in document.query_all('script'):
            src = script.get('src')
            if src:
                code = network_fn(src)
            else:
                code = script.text_content()
            self.engine.evaluate(code)
```

**Step 2：DOM Bindings（`js/dom_api.py`，约 400 行）**
```python
from PyQt6.QtCore import QObject, pyqtSlot

class JSDocument(QObject):
    @pyqtSlot(str, result='QVariant')
    def querySelector(self, selector):
        elem = css_select_one(self.document, selector)
        return JSElement(elem) if elem else None

    @pyqtSlot(str, result='QVariant')
    def createElement(self, tag):
        return JSElement(Element(tag))

class JSElement(QObject):
    @pyqtProperty(str)
    def innerHTML(self):
        return serialize_html(self.node)

    @innerHTML.setter
    def innerHTML(self, html):
        new_children = parse_html_fragment(html)
        self.node.children = new_children
        self.trigger_rerender()   # 触发重新布局
```

**Step 3：XHR 桥接（`js/xhr.py`，约 100 行）**
```python
class JSXHR(QObject):
    @pyqtSlot()
    def send(self):
        # 异步 HTTP 请求，完成后调用 onreadystatechange
        QTimer.singleShot(0, self._do_request)
```

---

## 工作量估算

| 模块 | 代码量 | 工作量 |
|------|--------|--------|
| QJSEngine 集成（语言层） | ~50 行 | 0.5 天 |
| DOM Bindings Tier 1（读写） | ~300 行 | 2 天 |
| DOM Bindings Tier 2（结构修改 + classList） | ~150 行 | 1 天 |
| innerHTML 解析插入 | ~50 行（复用 parser） | 0.5 天 |
| XHR 桥接 | ~100 行 | 1 天 |
| 重渲染触发（layout invalidation） | ~50 行 | 0.5 天 |
| 事件系统 Tier 3 | ~200 行 | 2 天 |
| **合计（Tier 1+2，可见内容）** | **~700 行** | **~5 天** |
| **合计（含 Tier 3 交互）** | **~900 行** | **~7 天** |

---

## 实现优先级总结

```
优先级 1（约 700 行，5 天）：
  QJSEngine 集成 → DOM 读写 API → innerHTML → XHR → 重渲染触发
  预期效果：页面内容从 10% 增加到 60-70%

优先级 2（约 200 行，2 天）：
  事件系统 → classList → setTimeout
  预期效果：tab 切换、下拉菜单等交互基本可用

可以不实现（stub 即可）：
  Analytics、IntersectionObserver、MutationObserver、localStorage
```
