# rENDER HTML5 Scope（范围定义书）

> 本文件回答一个问题：当 `rENDER` 自称"支持 HTML5"时，**到底覆盖哪些规范、哪些版本、哪些模块、哪些 API**。
>
> 这是一份"可证伪"的 scope —— 任何写入 IN SCOPE 的能力都必须有验收测试；任何写入 NON-GOAL 的能力遇到时一律静默降级或报错并退出，不得隐式特判。
>
> **本次重写：2026-09-27。** 上一版写于 2026 年初的 Python + PyQt6 原型期，全文的文件路径、
> 测试路径和若干状态判断已随架构迁移失效。本次重写把每一条状态都对齐到当前的 Rust 多 crate
> 引擎，并对齐到提交 `2bd8e8c` 加当日在途改动。**上一版里凡是依赖 PyQt 或 Python 能力边界的
> 推理，本次已整段删除而不是加注保留** —— 留着会让错误推理继续留在读者脑子里。
> 视觉层面的 file:line 证据见 `docs/visual_fidelity_gaps.md`，本文不重复推导。
>
> ⚠️ **快照与漂移**：写作期间有并行 agent 在 `render-core` 内落地了 HTML §15 UA 样式表、
> `text-decoration` / `text-shadow` / list marker 的绘制接线。本文已包含这些改动并标注了
> 它们落地的位置；`docs/visual_fidelity_gaps.md` 的 S2（UA 表 28 行）与 S3（含这三项）、
> S5（内联 SVG 未解析）**已落后于代码**。本文的状态以各节 `path:line` 为准。
> **S1（字体轴）仍然成立**。

> 配套文档：
> - 通用待办（项目法）：`docs/generic-browser-todo.md`
> - 视觉保真缺口证据：`docs/visual_fidelity_gaps.md`
> - 能力矩阵与验收基线：`docs/html5_gap_matrix.md`
> - 站点侧实机诊断：`docs/qq-compatibility-analysis.md`
> - 产品分层与迁移目标：`docs/rust_migration.md`
> - test262 门禁：`docs/test262.md`
> - WPT 运行方式：`docs/wpt.md` —— ⚠️ 该文件第 27 行仍写
>   `python tools/run-wpt-reftests.py`，**该脚本不存在**，真实入口见本文第 10 节
> - 测试策略：`docs/testing_strategy.md` —— ⚠️ **该文件整体属于已删除的 Python 工程**
>   （`tests/browser_visual_regression.py`、`tests/test_modern_rendering_contracts.py`、
>   `hao123_modules` 均不存在）。分层思路（单元 → 契约 → 浏览器差异）仍然成立，
>   但**路径与脚本名不可引用**
> - 路线图：`docs/html5_browser_full_plan.md` —— 36 周排期文档，
>   其中的里程碑定义仍被本文第 1 节引用，但其中的文件路径属于 Python 时代
> - 历史方案：`docs/rendering_paradigm_upgrade.md` —— 已完成的迁移方案，
>   顶部有历史横幅，正文不得作为当前引擎的证据引用

---

## 0. 用语约定

每条能力都打一个标签，全文统一：

| 标签 | 含义 |
|------|------|
| ✅ DONE | 当前代码已具备且有回归测试或外部套件记录 |
| 🟡 PARTIAL | 已实现但有已知缺口、范围明显小于规范，或有已记录的偏差 |
| 🔵 PLANNED | 在 scope 内，尚未实现（按 `docs/generic-browser-todo.md`，是待实现项，不是可绕过的项） |
| ⛔ NON-GOAL | 明确不做，遇到时按第 11 节"未实现策略"处理 |
| ❓ UNVERIFIED | 本次没有找到可引用的证据，不做断言 |

"未实现策略" 在第 11 节统一定义。

**当前引擎的权威 crate 布局**（本文所有 `path:line` 都以此为准）：

| crate | 职责 |
|-------|------|
| `crates/render-dom` | `Document` / `Element` / `Text` 树、命名空间、变更日志 |
| `crates/render-html` | HTML5 tokenizer + tree builder + 编码嗅探 + 序列化 |
| `crates/render-css` | 样式表解析、选择器、cascade、computed value、typed value、长度与颜色语法 |
| `crates/render-layout` | 格式化结构 + block/inline/flex/grid/table 求解器 + fragment 几何 |
| `crates/render-js` | JS lexer/parser/runtime/builtins + `<video>` 媒体栈 |
| `crates/render-core` | `page.rs` 的 `Page` 管线（协调一个 `Document`、一个 JS realm、一个事件循环与一个失效游标）、`document.rs`、`event_loop.rs`、`interaction/`、`image.rs`、绘制（`paint/display_list.rs` + `paint/raster.rs`）、`spec/registry.rs` 能力登记表 |
| `crates/render-net` | 有界 HTTP(S) 传输、批次、cookie jar、诊断 |
| `crates/render-browser` | 原生桌面壳（winit + softbuffer）、私有缓存、字体后端、渲染/资源工作线程 |

`render-core` 通过 `pub use` 再导出各基础 crate（`crates/render-core/src/lib.rs:7-32`），
所以 `render_core::css` / `::dom` / `::html` / `::js` / `::layout` 都是转发，不是重复实现。

---

## 1. 总目标分级

`rENDER` 的"HTML5 支持"分三档发布目标，每档都是独立可交付，**不允许跨档抢功能**。

> 上一版给每一档标了"约 16 / 24 / 36 周"的工期。那是 Python 原型期的排期估算，
> 引擎重写后已无意义，**本次删除**，只保留分档目标与验收判据。判据全部改写成
> 当前真实存在的套件。

### M1 — 静态 HTML5 文档可读

> 目标：能正确渲染没有动态脚本依赖的现代页面（新闻正文、文档、博客、表单展示）。
> 验收：WPT `html/`、`css-2d/`、`css-flexbox/` 子集有基线；`crates/render-core/tests/`
> 下的布局/绘制回归集无已知失败。
> **当前状态：未达成。** 阻碍不是解析或布局算法，而是**字体轴**与**伪元素**：
> `font-weight`/`font-style`/`font-family` 无消费者（`docs/visual_fidelity_gaps.md` S1，
> 且因为 UA 样式表现在已按 HTML §15.3 写对这些规则而更刺眼，见 3.3.1），
> `::before`/`::after` 无盒子生成（4.1）。
> **UA 样式表本身已不再是缺口** —— `docs/visual_fidelity_gaps.md` S2 说它是 28 行，
> 该描述已过期；现在是 142 行的 HTML §15 表（`crates/render-core/src/document.rs:70-211`）。
> 在字体轴修好之前，"静态文档可读"不成立，无论 WPT 数字如何。
> ⚠️ 验收判据里的"`example/` 静态样本页结构断言"**目前不存在** —— `example/` 只有
> `index.html` / `hao123.html` / `hao123_2003.html` / `hn.html` 四个 fixture，
> 没有对应的断言测试。上一版写的 `tests/test_modern_rendering_contracts.py`
> 属于已删除的 Python 工程。该判据是待补的测试目标，不是已有能力。

### M2 — 脚本驱动页面可交互

> 目标：常见门户/资讯页面初始化脚本可跑、事件可派发、DOM 可改、定时器/Promise/fetch 可工作。
> 验收：事件循环、DOM 变更、事件派发、fetch/XHR 的 Rust 回归集通过；
> `crates/render-js/` 单测与 test262 基线不回退。
> **当前状态：引擎侧能力大部分已具备**（见第 6、7 节），**但端到端仍不达成**：
> `.diag/qq/GAP_REPORT.md` 记录的 1364 次 `console.error`、主 bundle 抛
> `getProto: not an object`（其中 `document.cookie` 一项已修复，见 7.4），
> 以及 `HANDOFF.md` 京东一节记录的"样式表已取回但在线渲染为裸文本"，都说明
> 单项能力存在 ≠ 页面可用。

### M3 — 现代 HTML5 应用基本可用

> 目标：单页应用类页面（中等复杂度）能加载、能路由、能与后端通信。
> 验收：History API / fetch / 基础 Custom Elements 测试通过。
> **当前状态：部分达成。** `history.pushState`/`replaceState` 更新文档 URL 与
> `history.state`（同源检查见 `crates/render-js/src/runtime/mod.rs`），壳侧把请求记入会话
> 历史（`crates/render-browser/src/app.rs` 的 `drain_history_requests`），不触发加载；
> `back`/`forward`/`go` 重新加载所到达的条目。仍缺：`popstate` 事件、遍历时恢复
> `history.state`、`history.length` 恒为 1、`scrollRestoration`；`customElements` 不存在；
> Shadow DOM 不存在。

任何超出 M3 的能力（WebGL、Service Worker、IndexedDB、媒体解码等）属于本文档以外的
"未来路线"。

---

## 2. 标准版本锚定

不锚定版本就没有验收依据。本项目按以下规范快照对齐：

| 域 | 锚定标准 | 说明 |
|----|----------|------|
| HTML | WHATWG HTML Living Standard | 树构造按 13.2.6 分节实现并逐条注释引用（`crates/render-html/src/tree_builder.rs:1074-1117,1237-1290,1600-1860`） |
| DOM | WHATWG DOM Living Standard | `render-dom/src/lib.rs` |
| CSS | CSS Syntax 3 / Cascade 6 / Values 4 / Selectors 4 / Flexbox 1 / Grid 1 | 逐模块版本见第 4 节与 `crates/render-core/src/spec/registry.rs:139-210` |
| JavaScript | ECMAScript（以 test262 固定 revision 为准） | 见第 6.1 节与第 10 节；当前固定 revision 通过率见 `docs/html5_gap_matrix.md` |
| URL | WHATWG URL | `crates/render-js/src/runtime/builtins/url.rs`、`crates/render-core/src/navigation.rs` |
| Encoding | WHATWG Encoding | `crates/render-html/src/encoding.rs`，用 `encoding_rs::Encoding::for_label` 走完整 label 表（`:236,283`） |
| Fetch | WHATWG Fetch（子集） | `crates/render-js/src/runtime/builtins/fetch.rs` |

不在以上列表的标准（WebGPU、WebRTC、Web Authentication 等）一律 ⛔ NON-GOAL。

---

## 3. HTML 范围

### 3.1 解析与树构建

| 能力 | 状态 |
|------|------|
| HTML5 tokenizer（含错误恢复、CDATA、注释、DOCTYPE） | 🟡 `crates/render-html/src/tokenizer.rs`；RAWTEXT / RCDATA / script-data 三种内容模型（`:48,236`） |
| 树构建状态机（in body / in table / in select 等） | 🟡 `crates/render-html/src/tree_builder.rs`；隐式 `<p>` 关闭、list item / heading / 表格行隐式闭合均已实现（`:605-634`） |
| 隐式标签插入（`<html>`/`<head>`/`<body>`/`<tbody>`） | 🟡 `tree_builder.rs:1167-1185`（表格相关隐式插入） |
| 自动闭合与错位修复（misnested formatting elements） | 🔵 PLANNED（`<a>`/`<b>`/`<i>` 等活动格式元素的 adoption agency 未实现） |
| 字符引用 / 命名实体 / 数字实体 | ✅ |
| **Foreign content（内联 SVG / MathML）** | 🟡 **本次重写新增记录。** 解析侧已实现：in-foreign-content 判定（`tree_builder.rs:130-183`）、`math`/`svg` 起始标签建命名空间元素（`:556-567`）、MathML 文本积分点与 HTML 积分点（`:1237-1278`）、foreign content 规则与 breakout（`:1292-1400,1851-1860`）、XLink/XML/XMLNS 命名空间与 SVG/MathML 标签名与属性名调整表（`:1600-1619,1657,1787-1830`）。**渲染侧为零**：`crates/render-layout` 与 `crates/render-core/src/paint` 检索 `Namespace::Svg` 无命中，所以内联 `<svg>` 不产生任何几何。 |
| Template content（`<template>`） | 🟡 模板内容不参与样式表/脚本槽发现（`crates/render-core/src/document.rs` 的槽发现与 `src/script.rs:125` 都显式跳过 `template`），UA 样式表 `template{display:none}`（`document.rs:72-73`）；`DocumentFragment` 节点类型存在（`crates/render-dom/src/lib.rs:224,476`），但模板内容文档（`HTMLTemplateElement.content`）⛔ |
| `noscript` 的 scripting 分支 | 🔵 PLANNED。`noscript` 不在树构造器的文本内容元素白名单内（`tree_builder.rs:311,547` 只列 `style`/`noframes`/`xmp`/`iframe`/`noembed`/`script`/`title`/`textarea`），树构造器也不跟踪 scripting-enabled 标志（全仓库无该符号），同时 UA 样式表设 `noscript{display:none}`（`document.rs:75`）。三者叠加的结果是 `noscript` 回退内容不显示；该组合是否符合规范未经核实，**❓ UNVERIFIED** |
| 解析器错误回调 | 🟡 `HtmlParseErrorCode` + `HtmlParseDiagnostics` 已存在并在 `document.rs` 聚合计数（`.diag/qq/GAP_REPORT.md` §2.4 记为"只计数不打印"，**❓ 本次未核实当前是否已打印**） |

### 3.2 元素族（按使用频率，不按字母序）

| 类别 | IN SCOPE | OUT |
|------|----------|-----|
| 文档结构 | `html head body title meta link style script base` | — |
| 语义分块 | `header footer nav main section article aside h1-h6 hgroup address` | — |
| 段落与文本 | `p hr br pre blockquote div span` | — |
| 内联文本 | `a em strong code kbd samp var sub sup mark small b i u s wbr cite q dfn time` | `ruby rt rp`：UA 样式表给了 `display: ruby` / `ruby-base` / `ruby-text` / `ruby-text-container`（`crates/render-core/src/document.rs:112-115`）但**无 ruby 排版算法** ⛔ |
| 列表 | `ol ul li dl dt dd`（UA 样式表给了 `display` 值与 `ol/ul` 四级 `list-style-type`，`document.rs:139-156`）；**marker 绘制 ✅**（`crates/render-core/src/paint/display_list.rs:1298-1420`） | `list-style-image` 🔴；`@counter-style` ⛔ |
| 表格 | `table caption colgroup col thead tbody tfoot tr th td` | 🟡 CSS 2.1 §17 表格布局已实现（`crates/render-layout/src/solver/table.rs`，约 700 行 + 18 个测试），UA 样式表给了 `display` 值、`border-spacing:2px`、`td/th{padding:1px}`、`th{font-weight:bold}`、`caption{text-align:center}`（`document.rs:159-171`） |
| 表单 | `form input button select option optgroup textarea label fieldset legend output datalist` | `form method=dialog` ⛔ |
| 嵌入 | `img picture source figure figcaption` ✅（`picture` 源选择 `crates/render-core/src/image.rs:1053`；`srcset`+`sizes` ✅ `:987-1112,1148`） | `embed object applet` ⛔ |
| 媒体 | `<audio>` / `<video>` 元素状态、资源选择、解码管线骨架、呈现帧发布 | 🟡 像素解码 🔴：shipped `PlaceholderDecoder` 恒返回 `DecoderUnavailable`（`crates/render-js/src/video/mod.rs:40-41,58`），所以 `<video>` 只能显示 poster。demuxer/AVC 骨架在 `video/demuxer.rs`、`video/avc.rs` |
| iframe | `<iframe>` 参与布局，作为 raw-text 元素解析（`tree_builder.rs:547`） | 子文档加载 ⛔ |
| 交互 | `details summary`（UA 样式表给 `details > summary:first-of-type { display: list-item }`，`document.rs:210`） | `<dialog>` 模态语义 🔵：UA 样式表已给 `dialog:not([open]){display:none}` 与 `dialog{position:absolute;…}`（`document.rs:93-101`），但 `showModal()`/`close()` 与顶层渲染 🔴 |
| 编辑 | `contenteditable` 已读入并参与交互（`crates/render-browser/src/content_interaction.rs:323`） | 完整富文本编辑 🔵 |
| 脚本 | `<script>` `<template>` | `<script type=module>` 🟡：`render-js/src/parser.rs:583` 自述"Module declarations are intentionally lowered into the shared page"，`render-js/src/runtime/builtins/global_fns.rs:349` 自述"Module graph fetching belongs to the browser coordinator" —— 即**语法能解析、依赖图不做** 🔵 |
| 元数据 | `<meta charset>` `<meta http-equiv>` `<meta name=viewport>` `<base>` | 其他 meta ⛔ |
| 链接 | `rel=stylesheet` 执行 ✅（`document.rs:987-991`）；`rel=icon` 的处理 ❓ 本次未核实 | `preload` / `preconnect` / `canonical` / `alternate` ⛔ |

### 3.3 全局属性

🟡 `id class style title lang dir hidden tabindex` 反射到 DOM：`classList` 有
`add/remove/toggle/contains/item`（`crates/render-js/src/runtime/eval.rs:3270-3274`），
`hidden` 有 UA 规则（`crates/render-core/src/document.rs:72-73` 的 `[hidden]` 选择器）。
🔵 PLANNED：`data-*` ✅ 实际已实现（`dataset` 成员映射与读写删
`crates/render-js/src/runtime/builtins/dom.rs:386-401,775-820`）；
`role` / `aria-*` 🔴（无任何消费者，仅作为普通属性存在）。
⛔ NON-GOAL：`itemscope itemprop`（Microdata）、`is=""` 内置元素扩展。

### 3.3.1 UA 样式表

`crates/render-core/src/document.rs:70-211`（142 行）是按 WHATWG HTML "Rendering"
章节重写的用户代理样式表，逐节标注来源（§15.3.1 隐藏元素、§15.3.2 页面、§15.3.3 流内容、
§15.3.4 短语内容、§15.3.6 章节与标题、§15.3.7 列表、§15.3.8 表格、§15.3.10 表单控件、
§15.3.11 `hr`、§15.3.12 `fieldset`/`legend`、§15.5.5 `details`+`summary`），
并在 `document.rs:34-69` 的文档注释里把每一处引擎适配**显式标为能力缺口而非偏好**：

| 适配项 | 说明 | 对应缺口 |
|--------|------|---------|
| 逻辑属性全部物理化 | 规范写 `margin-block` / `padding-inline-start` / `inset-inline-start` / `border-inline-width`，布局求解器只消费物理 longhand，故逐条写物理值 | 表因此是 LTR 的；block 方向与 RTL 镜像需要先给 layout 加 `direction`（见 4.5 的 `direction: rtl` 行） |
| 字体轴声明保留但无消费者 | `font-weight`/`font-style`/`font-family`/`small-caps`/`text-transform`/`letter-spacing`/`text-indent`/`vertical-align` 到达 computed style 后无人读 | 见 4.5 的三条 🔴 |
| `q::before`/`q::after` 不生成 | 需要 generated content，引擎不建模 | `q` 规则只保留能兑现的排版提示 |
| quirks 规则不应用 | §15.3.9 的 margin collapsing quirks、`li` 内 `list-style-position` 默认值、表格字体重置都没做 | 见 3.5 与 S7 |
| `dialog` 用物理 `left`/`right` | 同逻辑属性一条 | — |

**quirks 模式本身未实现**：`document.rs:847-855` 对任何非 `NoQuirks` 文档发一条
`"CSS quirks are not implemented; standards-mode CSS semantics were used"` 诊断，
然后按 standards-mode 盒模型排。任何无 doctype 的页面因此排错。

### 3.4 表单

| 能力 | 状态 |
|------|------|
| `input` 类型 `text/password/email/url/tel/number/search/hidden/submit/reset/button/checkbox/radio` | 🟡 激活语义已实现（`crates/render-core/src/interaction.rs:1035-1060`），UA 样式表给了 `display:inline-block`、`font-size:13.3333px`、`box-sizing:border-box`、宽度/最小高度/内边距/边框，以及 `input[type=hidden]{display:none}` 与 `button`/非隐藏 `input` 的 `text-align:center`（`document.rs:74,176-187`） |
| `input` 类型 `file/range/color/date/datetime-local/time/month/week` | ⛔ NON-GOAL |
| 默认值 / disabled / readonly / required | 🟡 `disabled` 参与成功控件排除（`interaction.rs:891-893`）与 `:disabled` 匹配（`crates/render-css/src/selector.rs:672,1582-1592`）；未命名控件、无 `name` 控件、按钮/重置/文件/图像类型、未选中的 checkbox/radio、非 submitter 的 submit 也都正确排除（`interaction.rs:894-926`）；`readonly` / `required` 🔴 |
| `placeholder` | 🟡 `value` 为空时树构造器合成 placeholder 文本节点（`HANDOFF.md` 2026-09-27 记录，已实测）；`:placeholder-shown` 可解析（`selector.rs:675`）但状态恒不匹配（同 S6 动态伪类问题） |
| `<form>` 提交（GET，`application/x-www-form-urlencoded`） | ✅ 提交计划在 `interaction.rs:783-827`（含 `form` 属性关联、disabled 排除、成功控件收集），`submit` 事件可取消（`crates/render-browser/src/app.rs:2048-2069`），GET 导航在 `app.rs:3009-3017` |
| `<form>` 提交（POST） | 🔴 传输层支持（`crates/render-net/src/transport.rs:160,274,946`），但壳过滤掉非 GET 提交（`app.rs:3009`） |
| `multipart/form-data` | ⛔ NON-GOAL（无文件上传） |
| 约束验证 API（`checkValidity()` / `setCustomValidity()`） | 🔵 PLANNED |
| 表单关联 / `<form id>` 绑定 | ✅ `interaction.rs:828-856` |
| `<datalist>` 联想 | ⛔ NON-GOAL |

### 3.5 ⛔ NON-GOAL（HTML 部分）

Microdata、`is=""`、`<dialog>` 完整模态语义、Drag & Drop API、可访问性树、
`<meter>` `<progress>` 真实绘制、`<canvas>` 2D 上下文（`DisplayCommand::Canvas`
变体存在但无生产者，`crates/render-core/src/paint/display_list.rs:327`）、
内联 SVG 的几何渲染（解析已完成，见 3.1）。

---

## 4. CSS 范围

### 4.1 选择器

| 能力 | 状态 |
|------|------|
| 基本选择器（type/id/class/通用） | ✅ `crates/render-css/src/selector.rs:1169-1185` |
| 后代/子/相邻/通用兄弟 组合器 | ✅ |
| 属性选择器全集（`[a] [a=b] [a~=b] [a\|=b] [a^=b] [a$=b] [a*=b]`） | ✅ 7 种算子（`selector.rs:185-192,602-612`）+ `i`/`s` 大小写标志（`:627`）+ 匹配（`:1217-1245`） |
| 结构伪类（`:first-child :last-child :only-child :first/last/only-of-type :nth-child :nth-last-child :nth-of-type :nth-last-of-type :empty :root`） | ✅ `selector.rs:650-664` |
| 状态伪类（`:hover :focus :focus-visible :focus-within :active :checked :placeholder-shown :target :enabled :disabled`） | 🟡 **语法与匹配逻辑齐备，但状态从不接线。** 匹配读 `MatchContext`（`selector.rs:285-296`，`:1300-1340`）；在线渲染路径把 `focused`/`target` 置 `None`、`hovered`/`active`/`visited_links` 置空集（`crates/render-browser/src/render_worker.rs:495-508`）。结果：这些伪类**永不匹配**。这是 `docs/visual_fidelity_gaps.md` S6 同类问题的动态态版本 |
| 链接伪类（`:link :any-link :visited`） | 🟡 可解析可匹配（`selector.rs:669-671,1309-1311`），`visited_links` 恒空 ⇒ `:visited` 永不匹配 |
| 否定 `:not()`（接受选择器列表） | ✅ `selector.rs:667` |
| `:is() :where()` | ✅ `selector.rs:665-666`（`:where()` 0 特异性） |
| `:has()`（相对选择器） | ✅ `selector.rs:668` |
| `:lang()` | ✅ `selector.rs:682,1599-1605` |
| 伪元素 `::before ::after` | 🟡 **解析 ✅，渲染 🔴。** `selector.rs:539-584` 解析（含函数式伪元素），`selector.rs:1150` 按 `MatchContext.pseudo_element` 匹配；但没有伪元素盒生成 —— `render-layout` 与 `render-core/src/paint` 无相关代码，`render_worker.rs:499` 恒置 `pseudo_element: None`。见 `docs/visual_fidelity_gaps.md` S9（登记表也无此条目） |
| 伪元素 `::first-line ::first-letter ::placeholder ::marker` | 🔵 PLANNED |
| 选择器 4 其余 | 🔵/⛔ 按条目判定，不整体排除 |

### 4.2 级联与继承

| 能力 | 状态 |
|------|------|
| Origin & Importance（UA/User/Author/inline） | ✅ `crates/render-css/src/cascade.rs:136,926-940`；`!important` 反转 origin 优先级的测试在 `cascade.rs:986` |
| Specificity 计算 | ✅ `selector.rs` 的 `Specificity` |
| `@layer` 级联层（含 `revert-layer`） | ✅ `stylesheet.rs:186,217` 解析，`:500-528` 展开，`cascade.rs:877-924` 排序，`revert-layer` 测试 `cascade.rs:1175,1199` |
| `inherit / initial / unset / revert` | 🟡 关键字已定义（`crates/render-css/src/computed.rs` 的 initial 表 + cascade 的 `reverted_layers` 逻辑），逐关键字行为 ❓ 部分 UNVERIFIED |
| `:where()` 0 specificity | ✅ |
| Cascade origin 元数据保留（用于调试） | ✅ `cascade.rs:66-68,135-165` 保留 origin / layer / importance / layer_key |

### 4.3 值与单位

| 能力 | 状态 |
|------|------|
| 长度：`px em rem % vw vh vmin vmax` | ✅ `crates/render-css/src/properties.rs:498-499` |
| 长度：`svh lvh dvh` 家族与 `cqw/cqh/cqmin/cqmax` | ✅ 解析并在 `properties.rs:504-523` 求解（`small/large/dynamic` 与 container 上下文） |
| 长度：`ch ex` | 🔵 PLANNED |
| 长度：`pt pc cm mm in Q` | ⛔ NON-GOAL（仅打印场景） |
| 颜色：命名色 / `#rgb #rrggbb #rrggbbaa` / `rgb()` / `rgba()` / `hsl()` / `hsla()` | ✅ `properties.rs:1774-1788,2046` |
| 颜色：`color()` / `lab()` / `lch()` / `oklab()` / `oklch()` | ⛔ NON-GOAL |
| `currentColor` | ✅ `properties.rs:988,1009,1671` |
| `calc()`（+ - * /） | ✅ `properties.rs:2891,533-566` |
| `min() max() clamp()` | ✅ `properties.rs:568-585,2897,2944-2958` |
| `fit-content(<length-percentage>)` | ✅ `properties.rs:3108` |
| `var(--x, fallback)` 自定义属性 | ✅ `computed.rs:604,745,830-860`（含无效自定义属性记录与上限 `max_custom_properties: 4096`，`:168,178`）；`length.rs:324` 在长度表达式里也解析 `var()` |
| `env()` | ⛔ NON-GOAL |
| `attr()` | ⛔ NON-GOAL（仅 `content: attr()` 用法也 ⛔，因为 `content` 尚无消费者） |

### 4.4 盒模型与定位

| 能力 | 状态 |
|------|------|
| `display: block / inline / inline-block / none / list-item` | ✅ UA 样式表 `document.rs:71-210`（见 3.3.1）+ `crates/render-layout/src/tree.rs` 的格式化分类 |
| `display: flex / inline-flex` | ✅ `crates/render-layout/src/solver/flex.rs` |
| `display: grid / inline-grid` | ✅ `crates/render-layout/src/grid.rs`（轨道、`auto-fit`/`auto-fill`、`minmax`、`repeat`）+ `solver/grid.rs`（放置） |
| `display: table / table-row / table-cell / table-* -group / table-caption / table-column` | ✅ `solver/table.rs`（CSS 2.1 §17）+ `border-collapse`（`:586-588`）+ `vertical-align`（`:691`） |
| `display: contents` | ✅ `render-layout/src/tree.rs:814-827`（`contents` 盒透明，子节点保留） |
| `display: flow-root` | 🔴 无消费者 |
| `box-sizing` | 🟡 已解析并消费，❓ 覆盖范围未逐一核实 |
| `width / height / min-* / max-*` 含百分比、`auto`、`min-content/max-content/fit-content` | ✅ `solver/resolve.rs`；`aspect-ratio` ✅（`solver/block.rs:454`） |
| `margin` 含负值 | 🟡 负值可解析；**margin collapsing 🔴 完全未实现**（`solver/block.rs:258-259,411`，无 `max(+, −)` 步骤） |
| `padding / border` | ✅ 含 shorthand 展开（`crates/render-css/src/cascade.rs:399-404`） |
| `border-radius` 含两值椭圆 | 🟡 斜杠语法已解析（`crates/render-core/src/paint/display_list.rs:1817`），但 `parse_corner_radii` 内用 `f32::midpoint` 把水平/垂直半径平均为单一标量（`parse_radius_list` 在 `:1834`）⇒ **椭圆退化为圆**。这是对 CSS Backgrounds 3 §5.5 的已记录偏差 |
| `position: static / relative / absolute / fixed` | ✅ `solver/block.rs:765-782`（relative 平移子树、absolute/fixed 二次遍历定位） |
| `position: sticky` | 🔴 关键字在 `crates/render-css/src/properties.rs:758`，零消费者（`docs/visual_fidelity_gaps.md` S6） |
| `float` + 浮动清除 | ✅ `solver/block.rs`；`overflow:hidden` 自动包含浮动有回归测试 `crates/render-core/tests/layout_positioning.rs:119` |
| `overflow: hidden` 绘制裁剪 | ✅ `display_list.rs:709`（构造）+ `overflow_clip_shape` `:1702`；回归测试同文件 |
| 元素级滚动容器（可滚动溢出 + 滚动条） | 🔴 文档级视口滚动 ✅（`crates/render-layout/src/fragment.rs:128-175`、`crates/render-browser/src/chrome.rs:1812`），**元素级滚动条** 🔵 |
| `z-index` 与堆叠上下文 | 🟡 堆叠上下文只由 `transform` / `opacity<1` 创建（`display_list.rs:2179` 取 opacity、`:2189` `fragment_stacking_context`）。`z-index` 以裸文本读出（`crates/render-layout/src/solver/mod.rs:406-413`），只在 block 容器子级做**稳定排序**（`solver/block.rs:763`），排序键是"自身与所有后代 z-index 的最大值"这一启发式（`solver/mod.rs:414-425`）。绘制层无 z-index 概念；flex/grid 子项不参与排序；`z-index` 自身不创建堆叠上下文 |
| `clip-path` | 🔴 零消费者 |
| `contain` | ✅ `display_list.rs:1048` |
| `content-visibility` | 🔴 零消费者 |

### 4.5 文本与字体

> ⚠️ 本节是当前引擎**最差的一节**。上一版把它标成 🟡 "font 基础属性"，是低估。
> 真相是：`font-weight` / `font-style` / `font-family` 从 computed style 出发，
> 在到达字形之前就被丢弃。

| 能力 | 状态 |
|------|------|
| `font-size` | ✅ `crates/render-layout/src/solver/inline.rs:575`、`solver/resolve.rs:344` |
| `line-height` | ✅ `solver/inline.rs:580` |
| `font-weight` | 🔴 **物理上不可能。** `crates/render-css/src/cascade.rs:833-838` 把它写进交给测量/整形/绘制的 text style，但 `crates/render-layout/src/solver/mod.rs:33-36` 的 `TextStyle` 只有 `font_size` 与 `line_height`，没有接收字段；`crates/render-browser/src/font_backend.rs:33-43` 每个候选组只加载**一个**字体就 `break`，`:57-63` 只按字形覆盖选字体。⚠️ **UA 样式表已经写对了**：`b, strong { font-weight: bolder }` 与 `h1..h6 { font-weight: bold }` 都在 `crates/render-core/src/document.rs:105,130`，而该表自己的文档注释（`document.rs:56-62`）已声明这些声明"reach the computed style but no consumer downstream of render-core reads them"。规则正确、执行无效果 |
| `font-style` | 🔴 同上（斜体不可能）。UA 样式表已写 `cite, dfn, em, i, var, q, address { font-style: italic }`（`document.rs:91,104,125`），同样无消费者 |
| `font-family` | 🔴 同上（被忽略）。UA 样式表已写 `code, kbd, samp, tt` 与 `listing, plaintext, pre, xmp` 的 `font-family: monospace`（`document.rs:92,106`），同样无消费者 ⇒ 等宽块与正文字体完全相同 |
| `font-stretch` / `font-variant` / `font-feature-settings` / `font-variation-settings` | 🔴/⛔ |
| `white-space` | ✅ `solver/inline.rs:544,612` |
| `text-align: left / right / center` | ✅ `solver/inline.rs:446-454,456-467` |
| `text-align: justify` | 🟡 解析后按 `Start` 处理（`solver/inline.rs:462-467` 显式早退）⇒ 静默不生效 |
| `text-decoration`（含组合与五种线型 Solid/Double/Dotted/Dashed/Wavy） | 🟡 **已接线。** 生产 `crates/render-core/src/paint/display_list.rs:1164-1232`，`PaintPhase::TextDecoration`（`:342`），沿格式化祖先传播 `:1232-1296`，光栅化 `crates/render-core/src/paint/raster.rs:797-840`（按 CSS Text Decoration 3 §3.1 分线型）。⚠️ **传播是绘制侧近似**，`display_list.rs:1232-1238` 自述：沿格式化祖先取第一个指定该线的元素，中间内联上的 `text-decoration-line: none` 还关不掉装饰；正确修法是把传播移进 `render-css` |
| `text-shadow` | 🟡 **已接线。** 生产 `display_list.rs:1194-1207`、多层解析 `:2435-2450`、包围盒 `:2492`、光栅化 `raster.rs:756` |
| `list-style-type` / `list-style-position`（marker 绘制） | 🟡 **已接线。** `display_list.rs:1298-1420`（marker 几何由绘制而非布局拥有，`list-style-position: outside` 放内容边 leading 侧、`inside` 放内容原点）、`ListMarkerPaint` / `ListMarkerShape`（`:254-270`）、光栅化 `raster.rs:953-1015`（disc/circle/square 实心与空心、有序标记走文本整形） |
| `text-transform` / `letter-spacing` / `word-spacing` / `text-indent` / `text-overflow` / `word-break` / `overflow-wrap` | 🔴 零消费者（`text-indent` 仅初始值，`crates/render-css/src/computed.rs:77,80`） |
| `direction: rtl` + `unicode-bidi` 显示级 BiDi | 🔴 `direction` 只有初始值（`computed.rs:73`），`render-layout` 无消费者。`render-layout/src/geometry.rs:88-110` 有 `Direction` / `WritingMode` 类型与逻辑→物理换算，但**没有从 CSS `direction` 读入的通路** |
| `@font-face` + WOFF/WOFF2 | 🔴 解析后直接丢弃（`crates/render-css/src/stylesheet.rs:558-559`）；`render-core` 内检索 `@font-face` 无命中 ⇒ **全站无 webfont** |
| 复杂脚本整形（阿拉伯/印度系/泰文 cluster） | 🔵 PLANNED。⚠️ **上一版把它列为 ⛔ NON-GOAL，理由是"依赖 HarfBuzz，超出 PyQt 默认能力"。该理由随 Python/Qt 架构一同退役，本次删除整行理由。** 当前引擎的文本整形是自研的（`crates/render-core/src/paint/display_list.rs` 的 `TextShaper` trait + `ReferenceTextShaper` + `crates/render-browser/src/font_backend.rs` 的字形掩码），与 HarfBuzz 无关；缺口是真实的（无 cluster 成形、无 script run 分割），但它是**待实现项**，不是"外部库做不到因而放弃" |

### 4.6 视觉效果

| 能力 | 状态 |
|------|------|
| `background-color` | ✅ |
| `background-image (url)` | 🟡 单层图片 URL 可绘制；多层背景 🔵 |
| `background-repeat / position / size / clip / origin / attachment` | 🟡 `background-clip` 与 `border-box`/`padding-box` 差异已实现（`display_list.rs` 的 `background_clip_shape:1768` 及其调用点，回归测试同文件）；`background-origin`/`repeat`/`position`/`size`/`attachment` 的完整矩阵 ❓ 部分未核实 |
| `linear-gradient` | ✅ 解析 `properties.rs:1712-1720`、构造 `display_list.rs:1998`（`parse_linear_gradient`）、光栅化 `crates/render-core/src/paint/raster.rs`（`paint_linear_gradient`，测试同文件） |
| `radial-gradient` | 🔴 **`DisplayCommand::RadialGradient` 变体存在（`display_list.rs:286,326`）、光栅化分支存在（`raster.rs` 的 RadialGradient 臂）、诊断标签存在（`crates/render-browser/src/diagnostics.rs:44`），但解析器不认（`properties.rs:1712-1720` 只处理 `linear-gradient`），且无任何构造点。** 这正是 `docs/generic-browser-todo.md` 说的"解析了但没人消费"类缺陷 |
| `conic-gradient` / `repeating-*-gradient` | 🔴/⛔ |
| `box-shadow` | ✅ `display_list.rs:1903` 起解析多层阴影并构造命令 |
| `text-shadow` | 🟡 已接线，见 4.5 |
| `opacity` | ✅ `display_list.rs:2179`（读取 opacity 并决定是否建堆叠上下文） |
| `filter` / `backdrop-filter` | 🔴 零消费者 |
| `mask-*` | 🔴 零消费者 |
| `mix-blend-mode` / `background-blend-mode` | 🔴 零消费者 |

### 4.7 变换、过渡、动画

| 能力 | 状态 |
|------|------|
| `transform: translate / translateX/Y / scale / scaleX/Y / rotate / skew / matrix` | ✅ 端到端。类型在 `crates/render-css/src/properties.rs`（`TransformFunction` / `TransformList` / `TransformOrigin`），数学在 `render-core` 的 `Transform2D`，显示列表发射在 `display_list.rs:764-800`（`fragment_transform`，`transform` 读取在 `:771`），`PushTransform`/`PopTransform` 命令在 `:318-319`，光栅化有纯平移快路径与通用仿射离屏 warp（`raster.rs`）。`calc()` 参与 translate（`properties.rs` 的 transform 测试） |
| `transform-origin` | ✅ `display_list.rs:790-793` |
| `transition` | 🔴 零消费者 |
| `animation` + `@keyframes` | 🔴 `@keyframes` 块解析后丢弃（`stylesheet.rs:552-557`，注释自述"paint pipeline has no animation clock"），`render-core` 内检索 `keyframes`/`animation` 无命中 |
| `will-change` | ⛔ |
| Web Animations API | ⛔ |

### 4.8 媒体查询

| 能力 | 状态 |
|------|------|
| `@media` 媒体类型（`all` / `screen` / `print` / `speech`） | ✅ `crates/render-css/src/cascade.rs:335-337`（`print`/`speech` 恒 false） |
| `@media (min-width / max-width / width / min-height / max-height / height)` | ✅ `cascade.rs:350-375`；长度单位支持 `px`/`em`/`rem`/0（`:378-393`） |
| `@media (orientation)` | ✅ `cascade.rs:353-361` |
| `@media (prefers-color-scheme)` 等用户偏好特性 | 🔴 `cascade.rs:363` 的 `_ => return false` 使任何未列举特性恒不匹配 |
| `@media` + `and` 连接词的压缩写法 | ✅ `cascade.rs:297-331`（括号深度感知，正确处理 `(min-width:1140px)and (max-width:1299.9px)`） |
| Container Queries `@container` | 🔴 容器查询**单位**（`cqmin`/`cqmax`）已求解（`properties.rs:522-523`），但 `@container` 规则本身 🔴 |
| `@media print` 的打印语义 | ⛔ |

### 4.9 其他 At-rules

| 能力 | 状态 |
|------|------|
| `@media` | ✅ `stylesheet.rs:227,529-538`（可嵌套，`nested_media` 累积） |
| `@layer`（语句 + 块） | ✅ 见 4.2 |
| `@supports` | 🟡 **偏差。** 块被解析（`stylesheet.rs:244,539-551`），但条件被**丢弃**：`:548` 是 `let _ = query;`，嵌套规则无条件应用。源码注释自述"Property support is intentionally permissive until the computed-value registry grows a complete CSS.supports implementation"。这意味着 `@supports (display: none) { ... }` 也会生效 |
| `@import` | 🔴 不是"部分实现"：at-rule 分派只处理 `@layer`，其余走 `IgnoredAtRule` 并记一条 `@import is parsed but not evaluated yet` 诊断（`stylesheet.rs:180-191,561-566`） |
| `@font-face` | 🔴 丢弃，见 4.5 |
| `@keyframes` | 🔴 丢弃，见 4.7 |
| `@page` `@property` `@scope` `@counter-style` `@font-feature-values` | 🔴 走 `IgnoredAtRule` + 诊断 |
| CSS Nesting | 🔵（是否进入 scope 需在本文登记后再实现，见第 13 节） |

### 4.10 CSS 总体 ⛔ NON-GOAL 汇总

Color 4 / Color 5、Houdini、Container Queries（规则层）、Subgrid、Masonry、
Scroll-driven Animations、Anchor Positioning、View Transitions、Multi-column。

---

## 5. SVG / Canvas / 媒体

### 5.1 SVG

| 能力 | 状态 |
|------|------|
| `<img src="*.svg">` 静态栅格化 | ✅ **自研栅格化器**，不是外部库。`crates/render-core/src/image/svg.rs:1-18` 自述其子集：`rect` `circle` `ellipse` `polygon` `polyline` `line` + `path` 的 `M m L l H h V v C c S s Q q T t Z z`（`A` 弧退化为到端点的直线）、`<g>` 上的 `translate`/`scale`/`matrix`/`rotate`/`skewX`/`skewY`、`fill`/`stroke`（含 `none`）沿 `<g>` 继承、根尺寸按 `width`/`height` 解析百分比并回退 `viewBox` 再回退规范默认 300×150。子集外的渐变、text、clip、mask、内嵌图片、脚本一律不产生贡献。入口 `crates/render-core/src/image.rs:585-586` |
| SVG 作为 image 资源参与替换元素尺寸 | ✅ `render-layout/src/solver/block.rs` 的 `ImageResourceProvider`（接线见 `crates/render-core/src/lib.rs:38-42`） |
| 内联 `<svg>` 的**解析** | ✅ 见 3.1（foreign content 已在树构造器实现） |
| 内联 `<svg>` 的**几何渲染** | 🔴 `render-layout` 与 `render-core/src/paint` 都不认识 `Namespace::Svg` ⇒ 不产生任何几何，图标全部消失。做法已在 `docs/visual_fidelity_gaps.md` S5 与 `docs/qq-compatibility-analysis.md` P2-7 记录：把 svg 子树序列化回 SVG 文本喂给**已有的** `image/svg.rs`，注册为 image resource |
| SVG 完整规范（动画、滤镜、`<foreignObject>` 内容） | ⛔ |

### 5.2 Canvas

⛔ NON-GOAL：2D Context、`getContext("2d")`、`toDataURL`、ImageData、
WebGL/WebGL2、WebGPU 全部不做。`DisplayCommand::Canvas` 变体存在
（`display_list.rs:327`）但无生产者，不构成能力。

### 5.3 媒体

`<video>` / `<audio>`：元素状态机与呈现帧发布已落地
（`crates/render-js/src/video/present.rs`、`video/mod.rs`），demuxer 与 AVC 骨架在
`video/demuxer.rs` / `video/avc.rs`。**像素解码未写**：shipped 的 `PlaceholderDecoder`
恒返回 `VideoError::DecoderUnavailable`（`video/mod.rs:40-41,58`），所以目前只能显示 poster。
实施顺序（保留上一版的顺序判断，理由改为通用能力而非"PyQt 没有解码器"）：
HTTP Range 与资源状态 → HTMLMediaElement 状态机 → MP4 解复用 → H.264/AAC 解码与音频输出
→ A/V 同步与视频合成 → Media Source Extensions 子集。WebVTT、Picture-in-Picture、
完整自动播放策略排在基础点播链路之后。

---

## 6. JavaScript 范围

> ⚠️ 上一版第 6.3 节写"实现层面绑定 Python `re`"、6.1 节把整个语言层标成
> 🔵 PLANNED M2，并建议先不实现 JS 引擎。**这两条已随 Python 架构退役。**
> rENDER 现在有自研 JS 引擎（`crates/render-js/`，无任何 JS 第三方运行时），
> test262 门禁基线为 **30,808 / 98,096 变体通过（31.41%），264 桶，crash 22，timeout 20**
> （`crates/render-core/tests/test262-baseline.tsv`，由 `crates/render-core/tests/test262.rs:234`
> 的 `enforce_baseline` 把关，只查回退）。

### 6.1 语言版本

以 test262 固定 revision 的实际通过集为准，而不是某个 ES 版本的"应该支持"清单。
已确认具备：

| 能力 | 状态 | 证据 |
|------|------|------|
| `var` / `let` / `const` / 函数 / 闭包 / 作用域链 | ✅ | test262 桶 `language/*` |
| `try/catch`、可选 catch binding、默认参数初始化器、rest 参数 | ✅ | `HANDOFF.md` 2026-09-20 记录默认参数左到右求值（含 TDZ、抛错传播、与 rest 组合） |
| 对象/数组字面量、展开、解构 | ✅ | |
| 模板字符串 | ✅ | |
| 箭头函数（词法 `this`） | ✅ | `HANDOFF.md` 2026-09-20：`this` 由动态栈改为**环境绑定**，箭头词法 this |
| **完整 class 语义** | ✅ | `crates/render-js/src/runtime/class.rs`：构造器/方法/get/set/static/实例与静态字段/static 块/计算键/私有元素/extends/匿名类/`new.target`/`super` 三形式/`#x in`/`obj.#x` + class 早期错误 + `ClassFrame`/`PrivateScope` 链 |
| 私有字段 `#x` | ✅ | `runtime/class.rs` + 词法侧 `#name` token |
| `??` / `??=` / `&&=` / `\|\|=` | ✅ | `HANDOFF.md` 2026-09-20 |
| Unicode `XID` 标识符 | ✅ | 同上（依赖已在 Cargo.lock） |
| `Symbol`（含 13 个 well-known、`Symbol.for`/`keyFor`、符号键属性双轨） | ✅ | `crates/render-js/src/value.rs` + `runtime/eval.rs` |
| 迭代器协议（`@@iterator`、for-of、spread、解构、`Array.from`） | ✅ | `runtime/builtins/iterator.rs` |
| **Iterator helpers**（`map/filter/take/drop/flatMap/concat/chunks/windows` + 直接方法） | ✅ | `runtime/builtins/iterator.rs`；`HANDOFF.md` 2026-09-20 记录 `built-ins/Iterator` pass 12 → 356 |
| `Map` / `Set`（含迭代器） | ✅ | |
| `Promise`（含真实原型 then/catch/finally） | ✅ | `runtime/builtins/promise.rs`；`HANDOFF.md` 2026-09-16 记录 Promise 获得真实原型 |
| `Proxy` / `Reflect` | ✅ | `runtime/builtins/proxy.rs`（全陷阱路径接入） |
| TypedArray 家族 | ✅ | `runtime/builtins/typed_array.rs` |
| `Date`（TimeClip、`toISOString`、ISO `Date.parse`、`toJSON`） | ✅ | `HANDOFF.md` 2026-09-20 |
| **generator / async 函数体** | 🔴 **未实现。** `crates/render-js/src/parser.rs:581-583` 源码注释直陈"Treat async/generator declarations as ordinary functions. The runtime does not suspend generator frames, but accepting their syntax lets feature-detection and polyfill code load normally."。`HANDOFF.md` 2026-09-20 记录 `methods-gen-*` 一族约 1k 用例失败 |
| `BigInt` 字面量 | 🔴 38 个字面量用例失败（`HANDOFF.md` 2026-09-20 记录） |
| 模块（`import` / `export`） | 🟡 语法可解析（`crates/render-js/src/parser.rs:583`），依赖图与执行顺序不做（`runtime/builtins/global_fns.rs:349`） |
| 动态 `import()` | 🟡 全局 `import` 存在（`crates/render-js/src/value.rs:1455-1462`），语义为页面内解析 |
| 顶层 await | 🔴 |
| `Intl.*` / `Temporal` | 🔴 各自是独立大簇，test262 大量 Syntax/Reference 失败（`HANDOFF.md`） |

### 6.2 内置对象

✅ `Object`（含完整属性描述符、访问器、冻结/密封/扩展性、符号键）
`Array`（规范式惰性迭代 + `MAX_MATERIALIZED_ELEMENTS` 上限）`String`（含包装对象）
`Number` `Boolean` `Math` `JSON` `Date` `RegExp` `Error` 及各错误子类
`Map` `Set` `Symbol` `Promise` `Proxy` `Reflect` `Iterator` `console`
`Function`（含 `name`/`length` own 属性与 `prototype.constructor` 回填）。
🟡 `WeakMap` / `WeakSet` / `FinalizationRegistry` / `WeakRef` 的 test262 状态本次未核实。
⛔ `SharedArrayBuffer` `Atomics`（无多线程 worker，暂无使用场景）。

### 6.3 正则表达式

实现是自研引擎 `crates/render-js/src/regex.rs`（**不是** Python `re`，上一版该说法已删除）。
🟡 基础组、量词、字符类、锚点、命名捕获组可用；
`y` / `u` 完整属性类 / 全部 lookbehind / replace 的高级组引用的覆盖范围本次未逐一核实。

### 6.4 模块

| 能力 | 状态 |
|------|------|
| `<script>` 顺序加载 | 🟡 顺序与阻塞语义由 `crates/render-core/src/script.rs` 与 `crates/render-browser/src/scripts.rs` 承担；`app.rs` 的 `start_classic_scripts()` 是否仍以样式表完成为门控，**❓ UNVERIFIED**（`.diag/qq/GAP_REPORT.md` §2.2 记录过该缺陷） |
| `<script async>` / `<script defer>` | 🟡 `docs/html5_gap_matrix.md` 记为待精确建模（`docs/generic-browser-todo.md` Priority 4） |
| `<script type=module>` 依赖图 | 🔴 |
| 动态 `import()` | 🟡 页面内解析 |
| Import maps | ⛔ |

### 6.5 事件循环

✅ 真实事件循环模型在 `crates/render-core/src/event_loop.rs`：
`TaskSource` 分类（`:23`，含 `DomManipulation` / `Timer` / `Networking`）、
task queue 与 `queue_task`（`:344`）、microtask 队列与 `queue_microtask`（`:361`）、
`max_pending_microtasks` / `max_microtasks_per_checkpoint` 双重上限（`:70-72`，
超限返回 `ResourceLimitReached` 而不是静默截断，`:90-105`）、
`perform_microtask_checkpoint`（`:480-508`，任务与微任务共用同一 FIFO checkpoint，
嵌套微任务加入同一检查点）、每任务退栈后的 rendering opportunity（`:443-476`）。
定时器与动画帧：`setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` /
`requestAnimationFrame` / `cancelAnimationFrame` 装在 `crates/render-js/src/value.rs:2633-2652`。
`queueMicrotask` 由 Promise settle 路径驱动。

⛔ `requestIdleCallback`（`crates/render-js/examples/qq_bundle_probe.rs:175` 里的 polyfill
反证引擎不提供）、`MessageChannel`、完整 `structuredClone`。

---

## 7. Web API 白名单

**未列出的 API 一律 ⛔ NON-GOAL。** 即便规范存在、即便测试用例需要，也不进入实现。

### 7.1 DOM

| API | 状态 |
|------|------|
| `document.getElementById / getElementsByClassName / getElementsByTagName` | 🟡 查询内核统一走 `render-css/src/selector.rs` 的 `select_all`；各入口见 `crates/render-js/src/runtime/builtins/dom.rs:52,65` |
| `document.querySelector / querySelectorAll` | ✅ 同上（接受 4.1 选择器子集，含 `:is/:where/:not/:has/:nth-*`） |
| `Element.classList` | ✅ `add/remove/toggle/contains/item`（`runtime/eval.rs:3270-3274`） |
| `Element.className / id / tagName / attributes` | 🟡 `tagName`/`nodeName` ✅（`runtime/eval.rs:2815-2821`）；`attributes` 集合 🔵 |
| `Element.getAttribute / setAttribute / removeAttribute / hasAttribute` | ✅ |
| `Element.children / childNodes / firstChild / lastChild / parentNode / nextSibling / previousSibling` | 🟡 遍历 API 部分具备，❓ 逐项未核实 |
| `Node.appendChild / removeChild / insertBefore / remove / contains / cloneNode` | ✅ `runtime/eval.rs:3250-3254`；`cloneNode(deep)` 在 `runtime/builtins/dom.rs:616` |
| `Element.append / prepend / before / after / replaceWith / insertAdjacentHTML` | 🔴 未实现 |
| `Element.matches` | ✅ `runtime/eval.rs:3255`；`Element.closest` 🔴 |
| `Element.innerHTML / outerHTML / textContent` | ✅ `runtime/builtins/dom.rs:546,583`（走 HTML fragment 解析）；`innerText` 🔴 |
| `Element.dataset` | ✅ `runtime/builtins/dom.rs:386-401,775-820` |
| `Element.style`（CSSStyleDeclaration） | ✅ `getPropertyValue` / `setProperty` / `removeProperty` / `item`（`runtime/eval.rs:3260-3269`），后端是 `style` 属性本身（`runtime/builtins/style.rs:62,79,92`） |
| `document.styleSheets` / `insertRule` / `cssRules` | 🔴 |
| `Element.getBoundingClientRect` | ✅ `runtime/eval.rs:3257-3259` + `value.rs:1912-1915`；无几何时返回全零（`runtime/tests.rs:835`） |
| `document.createElement / createTextNode / createComment` | ✅ `runtime/builtins/dom.rs:116,178` + `CreateComment`（`HANDOFF.md` 2026-09-27 记录，为修 jQuery 1.6.4 特征检测而补） |
| `document.createDocumentFragment` | ✅ `runtime/eval.rs:3223-3225` + `runtime/builtins/dom.rs:126`；底层 `NodeKind::DocumentFragment`（`crates/render-dom/src/lib.rs:224,476`）与插入时展开（`:960-976`） |
| `document.createEvent` / `compareDocumentPosition` / `attributes` 的 `NamedNodeMap` | 🟡 入口存在（`runtime/eval.rs:3226-3231`），❓ 行为保真度未核实 |
| Range / Selection | 🟡 选择模型在引擎侧完整（`crates/render-core/src/interaction.rs:126-283`、`interaction/hit_test.rs`），**JS 侧 `Range`/`Selection` API 🔴** |
| `MutationObserver` | ✅ `runtime/builtins/observers.rs:183-350`（基于 DOM mutation journal，`runtime/builtins/dom.rs:1069`） |
| `IntersectionObserver` | 🟡 已实现（`observers.rs:60-171`），但它服务的布局信息是合成的，❓ 语义保真度未核实 |
| `ResizeObserver` | 🔴 反证：`crates/render-js/examples/qq_bundle_probe.rs:191-192` 自己 polyfill 了它 |

### 7.2 事件

| API | 状态 |
|------|------|
| `addEventListener / removeEventListener` | 🟡 **第三个参数被完全忽略**（`runtime/builtins/events.rs:106-127` 只读 type 与 callback），故 `{ once, capture, passive }` 一律无效。冒泡 ✅（`:163-168,236,283`，最后冒泡到 window） |
| `Event` / `CustomEvent` 构造、`bubbles` | ✅ `events.rs:58-92`；`stopPropagation` / `stopImmediatePropagation` / `preventDefault` 🟡（`preventDefault` 驱动壳的默认动作，`crates/render-browser/src/app.rs:2048-2069`；两个 stop 方法 ❓ 未核实） |
| 鼠标事件 `click / mousedown / mouseup / mouseover / mouseout / mousemove` | 🟡 指针事件到 DOM 事件的映射在 `crates/render-core/src/interaction/hit_test.rs`；❓ 逐类型未核实 |
| 键盘事件 `keydown / keyup` | 🟡 焦点模型在 `crates/render-core/src/interaction.rs:474-563`（`FocusNavigationDirection` / `FocusTransition`）；❓ JS 事件构造未核实 |
| 表单事件 `submit / change / input / focus / blur` | 🟡 `submit` ✅（`app.rs:2048-2069`，可取消）；`focus`/`blur` 🔴（因为焦点态从不写入 `MatchContext.focused`，见 4.1） |
| 触摸 / Pointer Events | ⛔ |
| Drag / Clipboard | ⛔ |
| Composition / IME 事件 | 🟡 输入法合成状态被跟踪（`app.rs:2923,3093` 有"合成期间不提交表单"的闩锁），完整 Composition 事件 ⛔ |

### 7.3 网络

| API | 状态 |
|------|------|
| `fetch(url, init)` | 🟡 `runtime/builtins/fetch.rs:16` 声明实现 `fetch()` 与 `Response`；❓ `init` 各字段（`credentials` / `redirect` / `signal` / `mode`）覆盖范围本次未核实 |
| `Response` | 🟡 同上 |
| `Request` / `Headers` | 🟡 `setRequestHeader` 在 XHR 侧存在（`fetch.rs:696`），独立 `Headers` 对象 ❓ 未核实 |
| `XMLHttpRequest` | 🟡 `open/send/setRequestHeader/getAllResponseHeaders/onload/onerror/responseText` 有（`fetch.rs:597-803`）；**同步 XHR 明确抛错**（`:729`：`synchronous XMLHttpRequest is not supported`）；`responseType=text\|json\|arraybuffer` ❓ 未核实 |
| `URL` / `URLSearchParams` | ✅ `runtime/builtins/url.rs` |
| `AbortController` / `AbortSignal` | 🟡 传输层有 `CancelToken`（`crates/render-net/src/transport.rs:1024` 的 `FetchError::Cancelled`），JS 侧 `AbortController` 🔴 |
| CORS | 🔴 `crates/render-net/src/lib.rs:3-6` 明确自述"deliberately below browser Fetch semantics … does not own … CORS"。`Access-Control-*` 全仓库无消费 |
| WebSocket / SSE / Beacon / WebTransport / WebRTC | ⛔ |

### 7.4 存储

| API | 状态 |
|------|------|
| `localStorage` / `sessionStorage` | 🔴 反证：`crates/render-core/examples/baidu_diag.rs:245-248` 把它们列进"存在性探测"名单，而 `render-js` 内无实现 |
| `document.cookie` | 🟡 **已实现（上一版标 🔵 PLANNED，已过时）。** getter `crates/render-js/src/runtime/eval.rs:2848-2849`，setter `:3339-3342`，jar `runtime/mod.rs:111-112,325-345`，回归测试 `runtime/tests.rs:2488-2503`。⚠️ **但 jar 只是 `name -> value` 的 map，没有 path / domain / Secure / SameSite 作用域**，上一版承诺的"遵守 path/domain/secure/SameSite"不成立 |
| 传输层 cookie jar（HTTP 侧） | 🟡 `crates/render-net/src/cookie.rs` 有独立 jar 与 Domain 过宽拒绝（`:277`）、重定向链上的 cookie 处理（`transport.rs:1186,608-609`）；与 JS 侧 jar 是否同一份 ❓ 未核实 |
| IndexedDB / Cache API / File API / FileReader / Blob | ⛔（`Blob` 仅作为 fetch body 占位存在于 `runtime/builtins/blob.rs`） |

### 7.5 路由 / 历史

| API | 状态 |
|------|------|
| `location.href / pathname / search / hash` | 🟡 `install_location` 在 `crates/render-js/src/value.rs:2654-2669`，`ObjectHost::Location`；`innerWidth`/`innerHeight` 读自 viewport（`runtime/eval.rs:2706-2707,2780-2781`）。hash 变更是否触发 `hashchange` ❓ 未核实 |
| `history.back / forward / go / pushState / replaceState` | 🟡 pushState/replaceState 更新 URL 与 `history.state`，不加载；back/forward/go 重新加载所到达的条目。`length` 恒 1（壳侧未回写列表长度）；遍历不恢复 `state` |
| `popstate` 事件 | 🔴 同文档遍历不触发（`pushState` 之后的 back 走重新加载） |
| `BroadcastChannel` | ⛔ |

### 7.6 Web Components

| 能力 | 状态 |
|------|------|
| `customElements.define` + 自动升级 | 🔴 反证：`crates/render-core/examples/baidu_diag.rs:248` 把它列进探测名单，`render-js` 内无实现 |
| Shadow DOM `attachShadow({mode})` | 🔴（`render-dom` 已命名空间就绪，`crates/render-dom/src/lib.rs:192,209,502`，但没有 shadow root 概念） |
| `<slot>` 投影 | 🔴 |
| `<template>` 内容文档 | 🔴（见 3.1） |
| `closed` shadow root / declarative shadow DOM | ⛔ |

### 7.7 其他

| API | 状态 |
|------|------|
| `requestAnimationFrame` / `cancelAnimationFrame` | ✅ `value.rs:2639-2643` + `runtime/types.rs:203`（由宿主每帧驱动一次） |
| `window.matchMedia` | 🔴 |
| `performance.now()` | ✅ `value.rs:2809-2832` |
| `performance.timeOrigin` / `performance.timing.navigationStart` | ✅ `value.rs:2833-2849`（`timeOrigin` 恒 0，❓ 是否诚实未核实） |
| `performance.getEntries` / `getEntriesByType` | 🟡 存在（`value.rs:2850-2858`），返回内容 ❓ 未核实 |
| `navigator.cookieEnabled` / `navigator.onLine` | ✅ 但均恒 `true`（`value.rs:2754`） |
| `screen.colorDepth` 等 | ✅ 测试 `runtime/tests.rs:383` |
| `getComputedStyle` | ✅ `value.rs:1399` + `runtime/builtins/dom.rs:146` |
| `crypto.getRandomValues` | 🔴 |
| `alert` / `confirm` / `prompt` | 🔴。⚠️ 上一版写"实现为 PyQt 对话框"，该说法随 Qt 架构退役，本次删除。通用做法是走宿主抽象（`render-core/src/interaction.rs` 的 `DefaultActionKind` 家族已有同类扩展点） |
| `devicePixelRatio` | 🔴 硬编码 1.0（`crates/render-browser/src/app.rs:1464` 与 `crates/render-core/src/image.rs:109` 都是 `device_pixel_ratio_milli: 1_000`）。它目前只用于 `srcset` 密度选择（`image.rs:1085`），所以高分屏资源选择是错的 |
| `crypto.subtle` / WebAuthn / Notification / Geolocation / Battery / DeviceOrientation / Gamepad / Speech / WebMIDI / WebUSB / WebBluetooth / WebHID / WebSerial / WebNFC / `Intl.*` | ⛔ |

---

## 8. 网络与安全

### 8.1 协议

| 能力 | 状态 |
|------|------|
| HTTP/1.1 | ✅ `crates/render-net/src/transport.rs`（ureq 3 + rustls） |
| HTTPS 证书链与 SNI 验证 | ✅ 走 rustls 默认校验；`with_proxy` 注入点在 `transport.rs`（`HANDOFF.md` 2026-09-27 记录系统代理：`ureq::Proxy::try_from_env()` + Windows 注册表代理，测试 `crates/render-net/tests/proxy_transport.rs`） |
| HTTP/2 / HTTP/3 / QUIC | ⛔ |
| 重定向跟随与 303 POST→GET | ✅ `transport.rs:870-871`；⚠️ 最大跳数上限本次未核实 |
| 内容编码 | 🟡 **仅 gzip。** `transport.rs:694` 是 `accept_encoding("gzip")`。⚠️ 上一版写"br 通过 Python `brotli`，可选"，该说法随 Python 架构退役；当前是**不请求也不接受 br**，这是一个诚实的降级而不是缺失的实现 |
| 字符集嗅探 | ✅ `crates/render-html/src/encoding.rs`：`decode_html_bytes`（`:108`）、BOM/Transport/Meta/Fallback 四级来源（`HtmlEncodingSource`，`:52-56`）、BOM + meta 预扫描 1024 字节（`META_PRESCAN_BYTES`，`:12`）、`encoding_rs::Encoding::for_label` 完整 label 表（`:236,283`）、UTF-8/UTF-16BE/UTF-16LE/windows-1252/GBK/GB18030/Shift_JIS 等 |
| 连接复用 | 🟡 `HANDOFF.md` 2026-09-26 记录：ureq 3.3 + rustls 配置下池子失效，curl 同链路第二次 46ms 而引擎 135ms 不变；专项已派工，❓ 修复是否闭环未核实。测试骨架 `crates/render-net/tests/connection_reuse.rs` |
| 每地址回退 / happy-eyeballs | 🔴 `HANDOFF.md` 已派给 NET（`render-net/**`），❓ 未核实是否落地 |
| 批次超时可见性 | 🟡 30s 一次性 stall 上报（`HANDOFF.md` 2026-09-27）+ `crates/render-net/src/diagnostics.rs` |

### 8.2 同源与安全

| 能力 | 状态 |
|------|------|
| origin 判定（scheme + host + port） | 🟡 `render-core/src/navigation.rs` 有 `is_same_document_fragment_navigation` 等；CORS 未实现，故同源策略目前只在导航层生效 |
| CORS（Simple / Preflight / `Access-Control-*`） | 🔴 `render-net/src/lib.rs:3-6` 明确不负责 |
| 凭据传播规则（`credentials`） | 🔴 |
| Referrer Policy | ✅ **完整决策表**：`ReferrerPolicy` 九种取值（`navigation.rs:350-362`）、`StandardReferrerPolicy` 实现（`:373-410`）、默认 `strict-origin-when-cross-origin`（`:1057`） |
| Content-Security-Policy | 🔴 |
| 混合内容拦截 | 🔴 |
| Cookie SameSite/Secure/HttpOnly 语义 | 🔴 传输层 jar 有 Domain 校验（`crates/render-net/src/cookie.rs:277`），但 SameSite/Secure/HttpOnly 语义未实现；JS 侧 jar 连作用域都没有（见 7.4） |
| Subresource Integrity（`integrity`） | ⛔ |
| Trusted Types / Permissions Policy / COOP/COEP/CORP | ⛔ |
| 站点隔离 / 进程隔离 | ⛔（单进程） |
| 站点中立性机械检查 | ✅ `tools/check_site_neutrality.py`，在 `.github/workflows/rust.yml:53-54` 作为 CI 步骤执行（不需要构建，先于测试步骤失败） |

### 8.3 资源加载

- 并发限制：每 origin 有界（`crates/render-net/src/batch.rs:34` 的 per-origin 上限钩子，`:122-172` 的批次循环）；`NetworkWorker` 保留一个逻辑 CPU 给事件循环与操作系统（`crates/render-net/src/worker.rs:209`）。🟡
- 队列满背压：请求 park 为 `Deferred(fetch_handles)`，由 `poll_network` 每轮经 `submit_with_cancellation` 重试（`crates/render-browser/src/app.rs:1168-1196,1571-1628`）。✅ 这一项是 `HANDOFF.md` 记录的淘宝首帧 0 帧根因修复。
- 优先级：document > CSS > 同步脚本 > 字体 > 图像 > 异步/defer 脚本的完整优先级策略 🟡；现有的是"有界队列 + 活动页更大脚本预算 + latest-write-wins 渲染提交"。
- HTTP 缓存：私有内存 LRU（32 MiB，只接收显式 fresh 的匿名响应），过期条目带 `ETag`/`Last-Modified` 做条件重验证 🟡；有界 512 MiB 磁盘 store + 代际安全清理 + 校验和原子记录 🟡（`crates/render-browser/src/cache.rs`、`cache/disk.rs`、`cache/payload.rs`；read-through/write-back 仍分阶段接入）。
- 样式表批次重匹配：DOM 修订号变化时按 `requested_url` 重挂到新 plan 槽位（`HANDOFF.md` 2026-09-26 记录，`crates/render-browser/src/resources.rs` 的 `apply_stylesheet_batch_rematched`）✅

---

## 9. 国际化、可访问性、辅助

| 域 | 状态 |
|----|------|
| UTF-8 / GBK / GB18030 / Big5 / Shift_JIS / EUC-KR / ISO-8859-* / Windows-125x 解码 | ✅ `crates/render-html/src/encoding.rs`，经 `encoding_rs`，含 BOM + meta 预扫描 + 传输层 label + `windows-1252` 回退（`:36,45`）。⚠️ 上一版写"依赖 Python codecs"，该说法随 Python 架构退役 |
| 中日韩换行（UAX#14 简化版） | 🔴 断行由自研测量器承担（`crates/render-browser/src/font_backend.rs` 的字形掩码 + `render-layout/src/solver/inline.rs` 的断行），❓ 是否实现 UAX#14 断点未核实 |
| RTL 显示 BiDi（UAX#9） | 🔴 见 4.5 的 `direction` 条目 |
| 复杂脚本整形 | 🔵 见 4.5；上一版的 "⛔ NON-GOAL（依赖 HarfBuzz，超出 PyQt 默认能力）" 已删除 |
| ARIA 属性反射到 DOM | 🔴（`aria-*` 只是普通属性，无消费者） |
| 可访问性树 / 屏幕阅读器对接 | ⛔ |
| 键盘导航：Tab 焦点环、Enter/Space 触发按钮 | 🟡 顺序焦点模型 ✅（`crates/render-core/src/interaction.rs:474-563`）；**可见焦点环 🔴**（`:focus` 永不匹配，见 4.1） |
| `Intl.*` | ⛔ |
| 高 DPI（device pixel ratio）一致绘制 | 🔴 恒 1.0（见 7.7） |
| 用户缩放 / 页面缩放 | ⛔ |
| 打印（`window.print`、@page） | ⛔ |

---

## 10. 测试与验收基线

> ⚠️ 上一版本节的全部路径属于已删除的 Python 工程（`tests/test_*.py`、
> `tests/wpt/`、`tests/test_modern_rendering_contracts.py`、
> `tests/browser_visual_regression.py`）。本次按当前真实套件重写。
> **`docs/generic-browser-todo.md` 禁止删除测试，所以这些不是"被删了"，而是
> "从未存在于 Rust 树中"；上一版把它们写成已有测试是错的。**

测试矩阵（当前全部存在）：

1. **单元 / 集成测试**（`cargo test --workspace`）：
   - `crates/render-core/tests/dynamic_dom_render.rs` —— JS 改 DOM 后重新渲染
   - `crates/render-core/tests/layout_paint_regressions.rs` —— 布局与绘制回归
   - `crates/render-core/tests/layout_positioning.rs` —— 定位、浮动包含
   - `crates/render-core/tests/paint_images.rs` —— 图片绘制（含 `<video>` 呈现帧）
   - `crates/render-core/tests/js_conformance.rs` —— ⚠️ 该文件首部自述"These cases are
     local reductions inspired by ECMAScript semantics. They are not imported test262
     cases and must not be reported as a test262 pass rate."（`:1-4`）
   - `crates/render-net/tests/{connection_reuse,connect_budget,local_transport,proxy_transport}.rs`
   - 各 crate 内的 `#[cfg(test)]` 模块（如 `crates/render-layout/src/solver/table_tests.rs`）
2. **test262 门禁**：`crates/render-core/tests/test262.rs` + `tests/test262-baseline.tsv`
   （264 桶，30,808 / 98,096 变体通过，31.41%）。固定 revision 由
   `tools/fetch-test262.sh` 取入 `third_party/test262`，CI 缓存。
   门禁语义是"桶内 pass 不得回退"（`test262.rs:234-274`），不是"全绿"。
   子集跑自动跳过门禁；`RENDER_TEST262_UPDATE_BASELINE=1` 重建基线。
3. **WPT reftests**：`crates/render-core/tests/wpt_reftests.rs`。
   ⚠️ 上一版写"运行 `tools/run-wpt-reftests.py`" —— **该文件不存在**。
   真实入口是 Rust 测试：外部 checkout 由 `tools/fetch-wpt.ps1` 取入
   （pin 在 `c7fdee80f3f17b4e9813964916afdfd57ace863f`，`wpt_reftests.rs:21`），
   通过 `RENDER_WPT_ROOT` / `RENDER_WPT_MANIFEST`（或 `RENDER_WPT_TEST` +
   `RENDER_WPT_REFERENCE` 单例）配置，测试默认 `#[ignore]`
   （`wpt_reftests.rs:88-90`，理由是"requires tools/fetch-wpt.ps1 and an official
   WPT manifest"），CI 用 `RENDER_WPT_REFTESTS=1` 打开
   （`.github/workflows/rust.yml:67-88`，允许 skip，skip 不得记为 pass）。
   ⚠️ 已知限制：`@import` 与 `url()` 资源在该 runner 内被标为 unsupported
   （`wpt_reftests.rs:356-358`）。⚠️ `docs/wpt.md:27` 仍写
   `python tools/run-wpt-reftests.py`，该文件已不存在，详见本文开头的配套文档清单。
4. **性能基线**：`cargo run --release -p render-browser --bin render-perf`。
   headless deterministic HTML→像素基准，输出 parse / first render / first visible /
   scroll 的 JSON 分布。CI：`.github/workflows/perf.yml`，push/PR 跑
   `--fixture generated` smoke（20 分钟超时），每周一与手动触发跑 `--fixture all`
   全量（45 分钟超时），两者都上传 JSON artifact。**必须先在同一台机器上记录基线**，
   阶段目标为 `first_visible` p95 相对基线下降 ≥ 30%。它不测网络、缓存、原生窗口呈现
   与 JS 执行。
5. **站点实机诊断**：`.diag/` 下的离线捕获 + `crates/render-*/examples/*_diag.rs`
   （`qq_bundle_probe` / `bilibili_diag` / `baidu_diag` / `hao123_diag` /
   `layout_chain_diag` / `css_probe` / `css_corpus` / `dom_dump` / `jquery_bisect`）。
   这些是**诊断工具，不是验收门槛**。`tools/ppm2png.py` 把 `RENDER_DUMP_FRAME`
   的 PPM 转 PNG。
6. **站点中立性**：`tools/check_site_neutrality.py`（CI 步骤）。

---

## 11. "未实现策略" 统一约定

遇到 ⛔ NON-GOAL 的语法 / API / 协议时，按以下规则处理，**禁止隐式特判某站点**：

- **CSS 未识别属性 / 值**：丢弃声明，不抛错。`@font-face` / `@keyframes` 走
  `ParsedRule` 分支后静默丢弃（`crates/render-css/src/stylesheet.rs:552-559`）；
  其他未知 at-rule 记一条 `@<name> is parsed but not evaluated yet` 的
  `capability_diagnostic`（`:561-566`）—— **出错可见、可定位**。
- **HTML 未识别元素**：按规范默认 `display` 渲染，未在 UA 样式表出现的元素走
  `render-layout/src/tree.rs` 的内联默认路径，子节点正常布局。
- **未实现 JS API**：以 `undefined` 返回，或抛 `TypeError` 并在消息里点名接口
  （例如 `runtime/builtins/events.rs:101` 的
  `incompatible EventTarget method receiver`、`fetch.rs:729` 的
  `synchronous XMLHttpRequest is not supported`）。
- **未实现协议 / 编码**：网络层抛错并以"资源加载失败"占位，整页不崩溃。
  资源级上限走显式错误而非静默截断（例：`ImageLimits`
  `crates/render-core/src/image.rs:521-563`；`MAX_MATERIALIZED_ELEMENTS`
  见 `HANDOFF.md` 2026-09-20）。
- **未实现媒体**：保留布局占位，显示 poster。
- **任何降级路径**禁止涉及 host / URL 字符串判断，并由
  `tools/check_site_neutrality.py` 在 CI 中机械检查。

---

## 12. 与现有代码的差距快照（2026-09-27）

> 上一版本节列的是 Python 文件与行数（`html/parser.py` 591 行等）。Python 引擎已删除，
> 这些行数已无意义；本次改为当前 crate 的**位置与状态**，不再给行数 —— 行数在并行
> 作业下每天漂移，写下来就是下一个误导来源。当日有并行 agent 在 `render-core` /
> `render-layout` / `render-html` / `render-js` / `render-css` / `render-net` /
> `render-browser` 内作业，**结论以各节给出的 `path:line` 为准，位置以本表的路径为准**。

| 模块 | 位置 | 状态 |
|------|------|------|
| HTML tokenizer + tree builder | `crates/render-html/src/{tokenizer,tree_builder,encoding,serialization}.rs` | 🟡 foreign content 已实现；misnested formatting（adoption agency）🔴；`noscript` scripting 分支 🔴 |
| CSS 语法 + 样式表 | `crates/render-css/src/{properties,length,stylesheet}.rs` | 🟡 token/escape 与复杂 at-rule 行为有缺口；`@font-face`/`@keyframes`/`@import` 丢弃；`@supports` 条件丢弃 |
| 选择器 | `crates/render-css/src/selector.rs` | 🟡 语法面很宽（7 种属性选择器算子 + 31 个伪类，含 `:has()`/`:is()`/`:where()`），但动态伪类状态从不接线 |
| Cascade / computed | `crates/render-css/src/{cascade,computed}.rs` | 🟡 origin + `@layer` + `revert-layer` 完整；CSS-wide keywords 逐项行为 ❓ 部分未核实 |
| 布局求解 | `crates/render-layout/src/solver/{block,inline,flex,grid,table,resolve}.rs` | 🟡 block/inline/flex/grid/table 五套上下文齐备；**margin collapsing 🔴**、sticky 🔴、RTL 🔴 |
| 格式化结构 | `crates/render-layout/src/{tree,fragment,geometry}.rs` | 🟡 匿名块与 `display:contents` ✅；**伪元素盒 🔴** |
| JS 引擎 | `crates/render-js/src/{lexer,parser,value,regex}.rs` + `runtime/` | 🟡 test262 31.41%；generator/async 🔴、BigInt 字面量 🔴、模块依赖图 🔴 |
| 事件循环 | `crates/render-core/src/event_loop.rs` | ✅ task/microtask/rendering opportunity 三段式齐备 |
| DOM 与 Web API | `crates/render-js/src/runtime/builtins/*.rs` | 🟡 域覆盖广；`addEventListener` options、Shadow DOM、`localStorage`、`matchMedia`、CORS 🔴 |
| 网络 | `crates/render-net/src/{transport,batch,worker,cookie,diagnostics}.rs` | 🟡 有界并发、背压、代理、重定向、gzip；br 🔴、每地址回退 ❓、连接复用 ❓ |
| 绘制 | `crates/render-core/src/paint/{display_list,raster,scene,color}.rs` | 🟡 六相绘制序 `PaintPhase`（`display_list.rs:336-344`）、`box-shadow`（`:1907`）、`text-shadow`、`text-decoration-*`、list marker、`contain`（`:1048`）、`object-fit`（`:1497,1621`）、`transform`（`:764`）、`opacity`（`:2179`）、圆角裁剪（`:1768,1817`）、`linear-gradient`（`:1998`）、`overflow:hidden` 裁剪（`:709,1702`）均可用；仍有 10 个属性零消费者；`RadialGradient`/`Canvas` 两种命令有类型与光栅化但**无生产者** |
| 交互 | `crates/render-core/src/{interaction.rs,interaction/hit_test.rs}` | 🟡 命中测试、选择、焦点、激活、提交计划齐备 |
| 能力登记 | `crates/render-core/src/spec/registry.rs` | 🟡 20 条 `FeatureDefinition` 全为 `SupportStatus::Partial`，外加 `fetch.runtime` 标 `Missing`（`:266-273`）且 `tests: &[]` —— **与 `runtime/builtins/fetch.rs` 的实际实现不一致**；且完全没有动画、`@font-face`、伪元素、表格、内联 SVG、视频解码、表单的条目（`docs/visual_fidelity_gaps.md` S9） |
| 桌面壳 | `crates/render-browser/src/*.rs` | 🟡 标签、地址栏、缓存、字体后端、渲染/资源工作线程齐备；**`app.rs` 的样式表→脚本门控 ❓ 未核实** |

---

## 13. 修改本文件的规则

1. 任何 ✅/🟡/🔵/⛔ 标签变更必须随同代码或测试改动一起提交，并在 PR 描述里给出
   `path:line` 证据。
2. **提升标签**（⛔→🔵→🟡→✅）需要在 PR 描述里给出"为什么进入 scope / 为什么算实现"。
3. **降级标签**需要在 PR 描述里给出"为什么放弃"。**降级不是删除**：
   `docs/generic-browser-todo.md` 的 "Priority -1: Gaps Are Implemented Forward, Never Removed"
   规定，缺口是待实现项，不得删除、不得用假近似冒充、不得特判绕过。
4. 引入新规范模块前，先在本文件登记，再写实现。
5. 任何"依赖某个库/某个平台做不到"的理由，在该依赖从架构中移除后必须**整段删除**，
   不能加注保留 —— 留着等于把错误推理留在读者脑子里。上一版关于 PyQt 字体整形与
   `QFontDatabase` 的两处推理已按此规则删除。
6. `❓ UNVERIFIED` 标签是允许的，但必须写明"要核实什么"。诚实标注 `❓` 优于自信的错断言。
7. 架构变化时，本文第 0 节的 crate 布局表、第 10 节的测试路径、第 12 节的差距快照
   必须同一次改动一起更新。
