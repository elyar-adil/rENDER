# rENDER HTML5 Scope（范围定义书）

> 本文件回答一个问题：当 `rENDER` 自称"支持 HTML5"时，**到底覆盖哪些规范、哪些版本、哪些模块、哪些 API**。
>
> 这是一份"可证伪"的 scope —— 任何写入 IN SCOPE 的能力都必须有验收测试；任何写入 NON-GOAL 的能力遇到时一律静默降级或报错并退出，不得隐式特判。
>
> 配套文档：
> - 路线图：`docs/html5_browser_full_plan.md`
> - 架构升级：`docs/rendering_paradigm_upgrade.md`
> - 通用待办：`docs/generic-browser-todo.md`
> - 测试策略：`docs/testing_strategy.md`

---

## 0. 用语约定

每条能力都打一个标签，全文统一：

| 标签 | 含义 |
|------|------|
| ✅ DONE | 当前代码已具备且有回归测试 |
| 🟡 PARTIAL | 已实现但有已知缺口或 Bug，需要补齐 |
| 🔵 PLANNED | 在 scope 内，按里程碑实现 |
| ⛔ NON-GOAL | 明确不做，遇到时按"未实现策略"处理 |

"未实现策略" 在第 11 节统一定义。

---

## 1. 总目标分级

`rENDER` 的"HTML5 支持"分三档发布目标，每档都是独立可交付，**不允许跨档抢功能**：

### M1 — 静态 HTML5 文档可读（约 16 周）

> 目标：能正确渲染没有动态脚本依赖的现代页面（新闻正文、文档、博客、表单展示）。
> 验收：WPT html/、css-2d/、css-flexbox/ 子集通过率 ≥ 70%；30 个静态样本页通过结构断言。

### M2 — 脚本驱动页面可交互（约 24 周累计）

> 目标：常见门户/资讯页面初始化脚本可跑、事件可派发、DOM 可改、定时器/Promise/fetch 可工作。
> 验收：M1 全部 + WPT dom/、html/semantics/forms/、html/webappapis/scripting/ 子集 ≥ 60%；样本池里含脚本的 15 个页面交互断言通过。

### M3 — 现代 HTML5 应用基本可用（约 36 周累计）

> 目标：单页应用类页面（中等复杂度）能加载、能路由、能与后端通信。
> 验收：M2 全部 + History API / fetch / 基础 Custom Elements 测试通过；选定 10 个 SPA 样本首屏可见。

任何超出 M3 的能力（WebGL、Service Worker、IndexedDB、媒体解码等）都属于本 scope 文档以外的"未来路线"，不在 36 周计划内。

---

## 2. 标准版本锚定

不锚定版本就没有验收依据。本项目按以下规范快照对齐：

| 域 | 锚定标准 | 说明 |
|----|----------|------|
| HTML | WHATWG HTML Living Standard，截至 2026-01 快照 | 仅"应用作者使用频率高"的章节 |
| DOM | WHATWG DOM Living Standard（同期） | 取代 DOM Level 3/4 的版本号 |
| CSS | CSS Snapshot 2023 | 各模块各自版本见第 4 节 |
| JavaScript | ECMAScript 2019（ES10） | 见第 6 节关于 ES2020+ 选用项 |
| URL | WHATWG URL | |
| Encoding | WHATWG Encoding（UTF-8 + GBK + GB18030 + Big5 + Shift_JIS + EUC-KR + ISO-8859-* + Windows-125x） | |
| Fetch | WHATWG Fetch 子集 | 见第 7 节 |

不在以上列表的标准（WebGPU、WebRTC、Web Authentication 等）一律 ⛔ NON-GOAL。

---

## 3. HTML 范围

### 3.1 解析与树构建

| 能力 | 状态 |
|------|------|
| HTML5 tokenizer（含错误恢复、CDATA、注释、DOCTYPE） | 🟡 PARTIAL |
| 树构建状态机（in body / in table / in select 等） | 🟡 PARTIAL |
| 隐式标签插入（`<html>`/`<head>`/`<body>`/`<tbody>`） | 🟡 PARTIAL |
| 自动闭合与错位修复（misnested formatting elements） | 🔵 PLANNED M1 |
| 字符引用 / 命名实体 / 数字实体 | ✅ DONE |
| Template content（`<template>`） | 🔵 PLANNED M3 |
| `noscript` 在脚本启用时按规范跳过 | 🔵 PLANNED M2 |
| 解析器错误回调（用于 WPT 兼容） | 🔵 PLANNED M2 |

### 3.2 元素族（按使用频率，不按字母序）

| 类别 | IN SCOPE | OUT |
|------|----------|-----|
| 文档结构 | `html head body title meta link style script base` | — |
| 语义分块 | `header footer nav main section article aside h1-h6 hgroup address` | — |
| 段落与文本 | `p hr br pre blockquote div span` | — |
| 内联文本 | `a em strong code kbd samp var sub sup mark small b i u s wbr cite q dfn time` | `ruby rt rp` ⛔ NON-GOAL（CJK ruby 不做） |
| 列表 | `ol ul li dl dt dd` | — |
| 表格 | `table caption colgroup col thead tbody tfoot tr th td` 🟡 | — |
| 表单 | `form input button select option optgroup textarea label fieldset legend output datalist` | `<form method=dialog>` ⛔ |
| 嵌入 | `img picture source figure figcaption` | `embed object applet` ⛔ |
| 媒体 | `<audio>` / `<video>` 元素状态、资源选择、分段加载、解码、播放和控制条 | 🔵 PLANNED；以 Bilibili 点播为端到端验收 |
| iframe | `<iframe>` 渲染为占位框（宽高/边框正确，不做内嵌文档加载） | 完整子文档加载 ⛔（M3 之后再议） |
| 交互 | `details summary` | `dialog` 🔵 PLANNED M3（仅 modal showModal/close） |
| 编辑 | `contenteditable=""` 显示为只读 | 真正编辑 ⛔ |
| 脚本 | `<script>` `<noscript>` `<template>` | `<script type=module>` 🔵 PLANNED M2 |
| 元数据 | `<meta charset>` `<meta http-equiv>` `<meta name=viewport>` `<base>` | 其他 meta ⛔ |
| 链接 | `rel=stylesheet/icon/preload/preconnect/canonical/alternate` 仅前两者执行 | 其他仅识别不实践 |

### 3.3 全局属性

✅ `id class style title lang dir hidden tabindex` 必须正确反射到 DOM；
🔵 PLANNED M2：`data-*`、`role`、`aria-*`（仅作为属性反射，不构建可访问性树）；
⛔ NON-GOAL：`itemscope itemprop`（Microdata）、`is=""` 内置元素扩展。

### 3.4 表单

| 能力 | 状态 |
|------|------|
| `input` 类型 `text/password/email/url/tel/number/search/hidden/submit/reset/button/checkbox/radio` | 🔵 PLANNED M2 |
| `input` 类型 `file/range/color/date/datetime-local/time/month/week` | ⛔ NON-GOAL（M3 之后） |
| 默认值 / disabled / readonly / required | 🔵 PLANNED M2 |
| `<form>` 提交（GET/POST，application/x-www-form-urlencoded） | 🔵 PLANNED M2 |
| `multipart/form-data` | ⛔ NON-GOAL（无文件上传） |
| 约束验证 API（`checkValidity()`、`setCustomValidity()`） | 🔵 PLANNED M3 |
| 表单关联 / `<form id>` 绑定 | 🔵 PLANNED M2 |
| `<datalist>` 联想 | ⛔ NON-GOAL |

### 3.5 ⛔ NON-GOAL（HTML 部分）

Microdata、Web Components 之外的 `is=""`、`<dialog>` 完整模态语义、Drag & Drop API、可访问性树、`<meter>` `<progress>` 真实绘制、`<canvas>` 2D 上下文、内联 SVG 完整规范（见第 5 节）。

---

## 4. CSS 范围

锚定 CSS Snapshot 2023，模块逐项给状态。**若一个模块未列出，则一律 ⛔ NON-GOAL。**

### 4.1 选择器

| 能力 | 状态 |
|------|------|
| 基本选择器（type/id/class/通用） | ✅ |
| 后代/子/相邻/通用兄弟 组合器 | 🟡 |
| 属性选择器全集（`[a] [a=b] [a~=b] [a\|=b] [a^=b] [a$=b] [a*=b]`） | 🔵 PLANNED M1 |
| 结构伪类（`:first-child :last-child :nth-child :nth-of-type :only-child :empty :root`） | 🔵 PLANNED M1 |
| 状态伪类（`:hover :focus :focus-visible :active :checked :disabled :enabled`） | 🔵 PLANNED M2（hover/focus 由事件驱动，不可常态匹配） |
| 链接伪类（`:link :visited`） `:visited` 仅样式不暴露 JS | 🔵 PLANNED M2 |
| 否定 `:not()`（接受复合选择器） | 🔵 PLANNED M1 |
| `:is() :where()` | 🔵 PLANNED M2 |
| 伪元素 `::before ::after` | 🔵 PLANNED M1 |
| 伪元素 `::first-line ::first-letter ::placeholder ::marker` | 🔵 PLANNED M2 |
| 选择器 4 其余（`:has()` `:nth-last-of-type` 等） | ⛔ NON-GOAL |

### 4.2 级联与继承

| 能力 | 状态 |
|------|------|
| Origin & Importance（UA/User/Author/inline） | 🟡 |
| Specificity 计算 | 🟡 |
| `inherit / initial / unset / revert` | 🔵 PLANNED M1 |
| `@layer` 级联层 | ⛔ NON-GOAL |
| `:where()` 0 specificity | 随 4.1 进入 |
| Cascade origin 元数据保留（用于调试） | 🔵 PLANNED 配合 `rendering_paradigm_upgrade.md` Style Graph 重构 |

### 4.3 值与单位

| 能力 | 状态 |
|------|------|
| 长度：`px em rem % vw vh vmin vmax` | 🟡（`vmin/vmax` 缺） |
| 长度：`ch ex` | 🔵 PLANNED M2 |
| 长度：`pt pc cm mm in Q` | ⛔ NON-GOAL（仅打印场景） |
| 长度：`svh lvh dvh` 等动态视口单位 | ⛔ NON-GOAL |
| 颜色：命名色 / `#rgb #rrggbb #rrggbbaa` / `rgb()` / `rgba()` / `hsl()` / `hsla()` | 🟡 |
| 颜色：`color()` / `lab()` / `lch()` / `oklab()` / `oklch()` | ⛔ NON-GOAL |
| `currentColor` | 🔵 PLANNED M1 |
| `calc(+ - * /)`，仅长度 + 数字 | 🔵 PLANNED M1 |
| `min() max() clamp()` | 🔵 PLANNED M2 |
| `var(--x, fallback)` 自定义属性 | 🔵 PLANNED M2 |
| `attr()` | ⛔ NON-GOAL（仅 `content: attr()` 用法 🔵 M2） |
| `env()` | ⛔ NON-GOAL |

### 4.4 盒模型与定位

| 能力 | 状态 |
|------|------|
| `display: block / inline / inline-block / none / list-item` | 🟡 |
| `display: flex / inline-flex` | 🟡 |
| `display: grid / inline-grid` | 🟡 |
| `display: table / table-row / table-cell ...` | 🟡 |
| `display: contents` | 🔵 PLANNED M2 |
| `display: flow-root` | 🔵 PLANNED M1 |
| `box-sizing` | 🟡 |
| `width / height / min-* / max-*` 含百分比、`auto`、`min-content/max-content/fit-content` | 🟡 → 🔵 M1 完成 |
| `margin` 含负值与折叠 | 🟡 |
| `padding / border` | 🟡 |
| `border-radius` 含两值椭圆 | 🟡 → 🔵 M1 修复 |
| `position: static / relative / absolute / fixed` | 🟡 |
| `position: sticky` | 🔵 PLANNED M2 |
| `float` + 浮动清除 | 🟡 |
| `overflow visible/hidden/scroll/auto`（绘制裁剪 + 内容裁剪，不实现滚动条交互） | 🔵 PLANNED M1 |
| `z-index` 与堆叠上下文 | 🟡 → 🔵 M1 完成 |
| `clip-path` | ⛔ NON-GOAL |
| `contain` `content-visibility` | ⛔ NON-GOAL |

### 4.5 文本与字体

| 能力 | 状态 |
|------|------|
| `font-family / size / weight / style / variant / line-height` | 🟡 |
| `font-stretch` | ⛔ NON-GOAL |
| `text-align` left/right/center/justify（justify 仅西文） | 🟡 |
| `text-decoration` 单值与组合 | 🟡 |
| `text-transform / letter-spacing / word-spacing / white-space / word-break / overflow-wrap` | 🔵 PLANNED M1 |
| `text-indent / text-shadow` | 🔵 PLANNED M1 |
| `direction: rtl` + `unicode-bidi` 显示级 BiDi（UAX#9） | 🔵 PLANNED M3 |
| 复杂脚本整形（阿拉伯/印度系/泰文） | ⛔ NON-GOAL（依赖 HarfBuzz，超出 PyQt 默认能力） |
| `@font-face` + WOFF/WOFF2 通过 `QFontDatabase` 注册 | 🔵 PLANNED M2 |
| `font-feature-settings / font-variation-settings` | ⛔ NON-GOAL |

### 4.6 视觉效果

| 能力 | 状态 |
|------|------|
| `background-color / background-image (url)` | 🟡 |
| `background-repeat / position / size / clip / origin / attachment` | 🔵 PLANNED M1（`fixed` 的 attachment 不做） |
| 多重背景 | 🔵 PLANNED M2 |
| `linear-gradient / radial-gradient` | 🔵 PLANNED M1 |
| `conic-gradient` / `repeating-*-gradient` | ⛔ NON-GOAL |
| `box-shadow / text-shadow`（含多重阴影） | 🔵 PLANNED M1 |
| `opacity` | 🟡 |
| `filter` | ⛔ NON-GOAL（`blur()` 之外都不做；`blur()` 也仅在 M3 评估） |
| `mask-*` | ⛔ NON-GOAL |
| `mix-blend-mode / background-blend-mode` | ⛔ NON-GOAL |

### 4.7 变换、过渡、动画

| 能力 | 状态 |
|------|------|
| `transform: translate / translateX/Y / scale / scaleX/Y / rotate / matrix` | 🔵 PLANNED M2 |
| `transform: skew / perspective / 3D 变换` | ⛔ NON-GOAL |
| `transform-origin` | 🔵 PLANNED M2 |
| `transition` | 🔵 PLANNED M3 |
| `animation` + `@keyframes` | 🔵 PLANNED M3（基本计时函数：linear/ease/cubic-bezier） |
| `will-change`（识别但不优化） | 🔵 M3 |
| Web Animations API | ⛔ NON-GOAL |

### 4.8 媒体查询

| 能力 | 状态 |
|------|------|
| `@media (min-width / max-width / width)` | 🔵 PLANNED M1 |
| `@media (orientation)` | 🔵 PLANNED M2 |
| `@media (prefers-color-scheme)` | 🔵 PLANNED M3 |
| `@media print` | ⛔ NON-GOAL |
| Container Queries `@container` | ⛔ NON-GOAL |

### 4.9 其他 At-rules

| 能力 | 状态 |
|------|------|
| `@import url(...)`（顶部） | 🟡 |
| `@font-face` | 🔵 PLANNED M2 |
| `@keyframes` | 🔵 PLANNED M3 |
| `@supports` | 🔵 PLANNED M2 |
| `@page` `@property` `@layer` `@scope` `@counter-style` `@font-feature-values` | ⛔ NON-GOAL |

### 4.10 CSS 总体 ⛔ NON-GOAL 汇总

Color 4 / Color 5、Houdini、Container Queries、Subgrid、Masonry、Scroll-driven Animations、Anchor Positioning、View Transitions、CSS Nesting（语法层 🔵 M3 评估，不做）。

---

## 5. SVG / Canvas / 媒体

### 5.1 SVG

| 能力 | 状态 |
|------|------|
| `<img src="*.svg">` 通过 `QSvgRenderer` 静态渲染 | 🔵 PLANNED M1 |
| 内联 `<svg>` 子集（`<svg> <g> <rect> <circle> <ellipse> <line> <path> <polygon> <polyline> <text>`，仅静态属性，无动画/滤镜/clipPath/mask/`<use>`） | 🔵 PLANNED M3 |
| SVG 完整规范（动画、滤镜、`<foreignObject>`） | ⛔ NON-GOAL |

### 5.2 Canvas

`<canvas>` 元素只参与布局；2D Context、`getContext("2d")`、`toDataURL`、ImageData、WebGL/WebGL2、WebGPU 一律 ⛔ NON-GOAL。

### 5.3 媒体

`<video>` / `<audio>` 当前仅有布局占位，但媒体播放已经进入项目目标。实施顺序为：HTTP Range 与资源状态、HTMLMediaElement 状态机、MP4/DASH 解复用、H.264/AAC 解码与音频输出、A/V 同步和视频合成，再补齐 Bilibili 所需的 Media Source Extensions 子集。WebVTT、Picture-in-Picture 和完整自动播放策略仍在基础点播链路之后。

---

## 6. JavaScript 范围

### 6.1 语言版本

锚定 **ECMAScript 2019（ES10）作为基线**，外加以下来自 ES2020+ 的"必须"扩展（现代框架启动期会用）：

| 来源 | 语法/特性 | 状态 |
|------|-----------|------|
| ES2015 | `let / const / class / for-of / 模板字符串 / 解构 / 默认参数 / 展开 / rest / 箭头函数 / Map / Set / Symbol / Promise / 迭代器协议 / 模块（仅静态 import 在 M2） / `Object.assign` 等` | 🟡 / 🔵 PLANNED M2 |
| ES2017 | `async / await` | 🔵 PLANNED M2 |
| ES2018 | 异步迭代、对象 rest/spread | 🔵 PLANNED M2 |
| ES2019 | `Array.flat / flatMap / Object.fromEntries / 可选 catch binding` | 🔵 PLANNED M2 |
| ES2020 | 可选链 `?.` / 空合并 `??` / `BigInt`（仅识别不要求精度） / `Promise.allSettled` / 动态 `import()` | 可选链/空合并 🔵 PLANNED M2；其他 🔵 M3 |
| ES2021+ | 数字分隔符 `1_000` / `String.replaceAll` / 逻辑赋值 `\|\|=` | 🔵 PLANNED M3 |
| ES2022+ | 顶层 await、类字段、私有字段 `#x` | ⛔ NON-GOAL |

### 6.2 内置对象

✅ / 🟡 / 🔵：`Object Array String Number Boolean Math JSON Date(只读) RegExp(基础) Error TypeError ReferenceError SyntaxError Map Set Symbol Promise console`。
⛔ NON-GOAL：`WeakRef FinalizationRegistry Proxy Reflect SharedArrayBuffer Atomics Intl.* WeakMap WeakSet`（最后两者 🔵 M3 评估）。

### 6.3 正则表达式

实现层面绑定 Python `re`：
- ✅ 基础组、量词、字符类、锚点、命名捕获组（M2）；
- ⛔ NON-GOAL：sticky `y`、unicode `u` 完整属性类、lookbehind 全部、replace 函数高级组引用（仅基本 `$1`）。

### 6.4 模块

| 能力 | 状态 |
|------|------|
| `<script>` 顺序加载、阻塞渲染语义 | 🟡 → 🔵 M2 |
| `<script async>` / `<script defer>` | 🔵 PLANNED M2 |
| `<script type=module>` 静态 `import / export` | 🔵 PLANNED M3 |
| 动态 `import()` | 🔵 PLANNED M3 |
| Import maps | ⛔ NON-GOAL |

### 6.5 事件循环（与 `rendering_paradigm_upgrade.md` 对齐）

✅ 已有 `js/event_loop.py` 骨架；M2 必须把以下落地：
- task queue（script / timer / network / user-interaction）
- microtask checkpoint（仅在 task 退栈或 rendering opportunity 触发）
- `setTimeout / setInterval / queueMicrotask / Promise.then`
- rendering opportunity（推动 style/layout/paint flush）

⛔ NON-GOAL：`requestIdleCallback`、`MessageChannel`、`structuredClone` 完整算法（仅复合对象浅拷贝 🔵 M3）。

---

## 7. Web API 白名单

**未列出的 API 一律 ⛔ NON-GOAL。** 即便规范存在、即便测试用例需要，也不进入实现。

### 7.1 DOM

| API | 状态 |
|------|------|
| `document.getElementById / getElementsByClassName / getElementsByTagName` | 🟡 |
| `document.querySelector / querySelectorAll`（接受 4.1 的选择器子集） | 🟡 |
| `Element.classList / className / id / tagName / attributes` | 🟡 |
| `Element.getAttribute / setAttribute / removeAttribute / hasAttribute / toggleAttribute` | 🟡 |
| `Element.children / childNodes / firstChild / lastChild / parentNode / nextSibling / previousSibling` | 🟡 |
| `Element.append / prepend / before / after / remove / replaceWith / insertAdjacent*` | 🔵 PLANNED M2 |
| `Element.cloneNode(deep)` | 🔵 PLANNED M2 |
| `Element.matches / closest` | 🔵 PLANNED M2 |
| `Element.innerHTML / outerHTML / textContent / innerText` | 🟡（innerText 行为对齐 🔵 M2） |
| `Element.dataset` | 🔵 PLANNED M2 |
| `Element.getBoundingClientRect`（基于已布局结果） | 🔵 PLANNED M2 |
| Range / Selection | ⛔ NON-GOAL |
| MutationObserver | 🔵 PLANNED M3 |
| IntersectionObserver / ResizeObserver | ⛔ NON-GOAL |

### 7.2 事件

| API | 状态 |
|------|------|
| `addEventListener / removeEventListener` 含 `{ once, capture, passive }` | 🔵 PLANNED M2 |
| `Event / CustomEvent` 构造、`stopPropagation / stopImmediatePropagation / preventDefault` | 🔵 PLANNED M2 |
| 鼠标事件 `click / mousedown / mouseup / mouseover / mouseout / mousemove` | 🔵 PLANNED M2 |
| 键盘事件 `keydown / keyup / keypress(deprecated 不做)` | 🔵 PLANNED M2 |
| 表单事件 `submit / change / input / focus / blur` | 🔵 PLANNED M2 |
| 触摸 / Pointer Events | ⛔ NON-GOAL |
| Drag / Clipboard | ⛔ NON-GOAL |
| Composition / IME 事件 | ⛔ NON-GOAL |

### 7.3 网络

| API | 状态 |
|------|------|
| `fetch(url, init)`（init: `method headers body credentials redirect signal`） | 🔵 PLANNED M2 |
| `Response` `Request` `Headers` 基础 | 🔵 PLANNED M2 |
| `XMLHttpRequest`（仅 `open/send/setRequestHeader/getAllResponseHeaders/onload/onerror/responseText/responseType=text|json|arraybuffer`） | 🟡 → 🔵 M2 |
| `URL` / `URLSearchParams` | 🔵 PLANNED M2 |
| `AbortController / AbortSignal` | 🔵 PLANNED M3 |
| WebSocket | ⛔ NON-GOAL |
| Server-Sent Events | ⛔ NON-GOAL |
| Beacon / WebTransport / WebRTC | ⛔ NON-GOAL |

### 7.4 存储

| API | 状态 |
|------|------|
| `localStorage / sessionStorage`（同步、字符串值、配额 5MB） | 🔵 PLANNED M2 |
| Cookie：`document.cookie` 仅在文档 origin 内读写，遵守 path/domain/secure/SameSite | 🔵 PLANNED M2 |
| IndexedDB | ⛔ NON-GOAL |
| Cache API | ⛔ NON-GOAL |
| File API / FileReader / Blob | ⛔ NON-GOAL（`Blob` 作为 fetch body 占位 🔵 M3） |

### 7.5 路由 / 历史

| API | 状态 |
|------|------|
| `location.href / pathname / search / hash`（读 + hash 写触发 `hashchange`） | 🔵 PLANNED M2 |
| `history.pushState / replaceState / back / forward / go / popstate` | 🔵 PLANNED M3 |
| `BroadcastChannel` | ⛔ NON-GOAL |

### 7.6 Web Components

| 能力 | 状态 |
|------|------|
| `customElements.define` + 自动升级 | 🔵 PLANNED M3 |
| Shadow DOM `attachShadow({mode})` 含 `open` 模式 | 🔵 PLANNED M3 |
| `<slot>` 投影 | 🔵 PLANNED M3 |
| `<template>` 内容文档 | 🔵 PLANNED M3 |
| `closed` shadow root / declarative shadow DOM | ⛔ NON-GOAL |
| Form-associated custom elements | ⛔ NON-GOAL |

### 7.7 其他

`requestAnimationFrame` 🔵 PLANNED M3（与 rendering opportunity 绑定）；
`window.matchMedia` 🔵 PLANNED M2（同步求值，不监听变化 listener，监听 🔵 M3）；
`performance.now()` 🔵 PLANNED M2；
`crypto.getRandomValues` 🔵 PLANNED M3；
`crypto.subtle / WebAuthn / Notification / Geolocation / Battery / DeviceOrientation / Gamepad / Speech / WebMIDI / WebUSB / WebBluetooth / WebHID / WebSerial / WebNFC` 全部 ⛔ NON-GOAL。
`alert / confirm / prompt` 🔵 PLANNED M2（实现为 PyQt 对话框）。

---

## 8. 网络与安全

### 8.1 协议

- HTTP/1.1（urllib 已具备）✅
- HTTPS：必须验证证书链与 SNI；不允许 `verify=False` 跳过 🟡 → 🔵 M1
- HTTP/2 / HTTP/3 / QUIC ⛔ NON-GOAL
- 重定向：3xx 跟随，最多 20 跳，POST→GET on 303 🟡 → 🔵 M1
- 内容编码：`gzip / deflate / br`（br 通过 Python `brotli`，可选；缺失时禁止接受该编码）🔵 PLANNED M2
- 字符集：UTF-8 默认，按 `<meta charset>` / HTTP `Content-Type` / BOM 嗅探（顺序与 HTML 规范一致）🔵 PLANNED M1

### 8.2 同源与安全

| 能力 | 状态 |
|------|------|
| 同源策略（origin = scheme + host + port） | 🔵 PLANNED M2 |
| CORS（Simple / Preflight / `Access-Control-*` 头）针对 fetch/XHR | 🔵 PLANNED M2 |
| 凭据传播规则（`credentials: include / same-origin / omit`） | 🔵 PLANNED M2 |
| Referrer Policy（`no-referrer / strict-origin-when-cross-origin` 默认） | 🔵 PLANNED M2 |
| Content-Security-Policy（识别并解析；至少阻止内联脚本若 `script-src` 不含 `unsafe-inline`） | 🔵 PLANNED M3 |
| 混合内容拦截（HTTPS 页面下 HTTP 子资源） | 🔵 PLANNED M2 |
| Cookie SameSite/Secure/HttpOnly 语义 | 🔵 PLANNED M2 |
| Subresource Integrity（`integrity` 属性） | ⛔ NON-GOAL |
| Trusted Types / Permissions Policy / COOP/COEP/CORP | ⛔ NON-GOAL |
| 站点隔离 / 进程隔离 | ⛔ NON-GOAL（rENDER 单进程） |

### 8.3 资源加载

- 并发限制：每 origin 6 个连接（与浏览器一致）🔵 PLANNED M2
- 优先级：document > CSS > 同步脚本 > 字体 > 图像 > 异步/defer 脚本 🔵 PLANNED M2
- 缓存：内存级 LRU，根据 `Cache-Control: max-age` 与 `ETag/Last-Modified` 条件请求 🔵 PLANNED M2
- 磁盘缓存 ⛔ NON-GOAL

---

## 9. 国际化、可访问性、辅助

| 域 | 状态 |
|----|------|
| UTF-8 / GBK / GB18030 / Big5 / Shift_JIS / EUC-KR 解码（依赖 Python codecs） | 🔵 PLANNED M1 |
| 中日韩换行（按 UAX#14 简化版） | 🔵 PLANNED M2 |
| RTL 显示 BiDi（UAX#9） | 🔵 PLANNED M3 |
| 复杂脚本整形（阿拉伯、印度系、泰文 cluster） | ⛔ NON-GOAL |
| ARIA 属性反射到 DOM | 🔵 PLANNED M2 |
| 可访问性树 / 屏幕阅读器对接 | ⛔ NON-GOAL |
| 键盘导航：Tab 焦点环、Enter/Space 触发按钮 | 🔵 PLANNED M3 |
| `Intl.*` 完整 API | ⛔ NON-GOAL |
| 高 DPI（device pixel ratio）一致绘制 | 🔵 PLANNED M2 |
| 用户缩放 / 页面缩放 | ⛔ NON-GOAL |
| 打印（`window.print`、@page） | ⛔ NON-GOAL |

---

## 10. 测试与验收基线

scope 内每条 🟡 / 🔵 都必须挂到至少一个测试套件，否则不允许合并实现。测试矩阵：

1. **单元测试**：`tests/test_*.py`，覆盖 parser / cascade / layout / event-loop 核心算法。
2. **WPT 子集**：在 `tests/wpt/` 引入以下子目录的快照（手工挑选 + 自动转换为 pytest）：
   - `html/syntax/`、`html/semantics/forms/`、`html/webappapis/scripting/`、`dom/nodes/`、`dom/events/`、`css/CSS2/`、`css/css-flexbox/`、`css/css-grid/`、`css/css-cascade/`、`url/`、`encoding/`、`fetch/api/basic/`、`fetch/api/redirect/`、`xhr/`。
   - 每个里程碑设最低通过率门槛（M1 70% / M2 60% / M3 50%）。
3. **页面契约测试**：`tests/test_modern_rendering_contracts.py` 等，30 个静态样本 + 15 个脚本样本 + 10 个 SPA 样本。
4. **视觉回归**：`tests/browser_visual_regression.py` 与 Chromium 对比，仅作参考、不作合并门槛。
5. **性能基线**：每周采集 TTFR、layout 次数、JS 执行时长；超阈值 10% 红线。

---

## 11. "未实现策略" 统一约定

遇到 ⛔ NON-GOAL 的语法 / API / 协议时，按以下规则处理，**禁止隐式特判某站点**：

- **CSS 未识别属性 / 值**：丢弃声明，记录到 `css.unsupported` 计数，不抛错。
- **HTML 未识别元素**：按 `display:inline` / `display:block`（依据规范默认）渲染，子节点正常布局。
- **未实现 JS API**：以 `undefined` 返回的属性，或抛 `TypeError("not implemented in rENDER: <name>")`，遵循"出错可见、可定位"。
- **未实现协议 / 编码**：网络层抛错并以"资源加载失败"占位，整页不崩溃。
- **未实现媒体**：保留布局占位，控件骨架显示"unsupported media"。
- **任何降级路径**禁止涉及 host / URL 字符串判断。

---

## 12. 与现有代码的差距快照（截至本文件提交时）

仅作参考，每月由 `docs/html5_gap_matrix.md` 自动刷新。

| 模块 | 行数 | 主要状态 |
|------|------|----------|
| `html/parser.py` | 591 | 🟡 容错与 in-table state 机有缺口 |
| `css/cascade.py` | 940 | 🟡 origin 元数据、`var()`、`@supports` 缺 |
| `css/selector.py` | 827 | 🟡 动态伪类常态匹配 Bug、`:nth-*` 待补 |
| `css/computed.py` | 187 | 🟡 提前 px 化破坏阶段语义（见 `rendering_paradigm_upgrade.md`） |
| `layout/block.py` | 722 | 🟡 margin collapsing、stacking context 不全 |
| `layout/inline.py` | 785 | 🟡 line-box 与 BFC 边界耦合 |
| `layout/flex.py` | 464 | 🟡 `flex-basis` 语义、min-content 约束 |
| `layout/grid.py` | 276 | 🟡 仅基础轨道与放置 |
| `js/interpreter.py + parser.py + lexer.py` | 2715 | 🟡 ES5 子集，无 class、async/await、ES2015 模块 |
| `js/event_loop.py` | 167 | 🟡 框架在，缺 task queue 分类与 rendering checkpoint |
| `js/dom_api.py` | 1054 | 🟡 查询/反射较全，事件派发待规范化 |
| `network/http.py` | 231 | 🟡 缺缓存、CORS、混合内容、重定向上限 |
| `backend/qt/painter.py` | 894 | 🟡 阴影、渐变、变换、SVG 待接 |

---

## 13. 修改本文件的规则

1. 任何 ✅/🟡/🔵/⛔ 标签变更必须随同代码或测试改动一起提交。
2. 提升标签（如 ⛔→🔵）需要在 PR 描述里给出"为什么进入 scope"。
3. 降级标签（如 🔵→⛔）需要在 PR 描述里给出"为什么放弃"。
4. 36 周路线图调整必须同步更新 `docs/html5_browser_full_plan.md`。
5. 引入新规范模块前，先在本文件登记，再写实现。
