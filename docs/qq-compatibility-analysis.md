# qq.com / taobao.com 兼容性分析报告

> 首次分析：2026-03-20，架构为 Python + PyQt6 原型（已退役）。
> 本次重写：2026-09-27，架构为 Rust 多 crate 引擎（`render-dom` / `render-html` /
> `render-css` / `render-layout` / `render-js` / `render-core` / `render-net` /
> `render-browser`），代码状态为提交 `2bd8e8c` 加当日在途改动。
> ⚠️ **快照与漂移**：写作期间有并行 agent 落地了 HTML §15 UA 样式表与
> `text-decoration` / `text-shadow` / list marker 绘制接线。本文已按落地后的状态书写。
> `docs/visual_fidelity_gaps.md` 的 S2、S3、S5 已落后于代码；**S1（字体轴）仍然成立**。
> 目标页面：https://www.qq.com 、https://www.taobao.com 、https://www.jd.com
> 原则：从零实现，correctness over completeness，每一行代码都要有价值。
> 硬约束：按 `docs/generic-browser-todo.md`，能力一律**通用实现**；本文任何建议都不得
> 引入站点特判、站点数据源替换或借用外部引擎渲染。

**现场证据来源（不要重新推导，直接引用）**

- `.diag/qq/GAP_REPORT.md`（2026-09-18，qq.com + taobao.com 实机诊断，含帧指标与日志行号）
- `HANDOFF.md`「京东在线问题精确诊断」一节（在线管线"有样式却裸文本"的时序证据）
- `docs/visual_fidelity_gaps.md`（视觉保真缺口的 file:line 证据与排序）

**状态标签约定（全文统一）**

| 标签 | 含义 |
|------|------|
| ✅ 已实现 | 当前代码具备，且有测试或实机证据 |
| 🟡 部分 | 有实现但范围明显小于规范，或有已记录的偏差 |
| 🔴 缺失 | 无消费者 / 无实现，页面按规范应出现的内容不会出现 |
| ⛔ 非目标 | 明确不做，遇到时按 `docs/html5_scope.md` 第 11 节的"未实现策略"降级 |
| ❓ 未核实 | 本次没有找到可引用的证据，不做断言 |

---

## 0. 本次重写修正了哪些已失效结论

旧版全文基于 Python + Qt 架构，下列结论**已被证伪或被超越**，不再作为证据使用。
列出它们本身就是本文档价值的一部分：读者若在其他旧报告里看到同样的说法，
应知道它是过时的。

| 旧结论 | 2026-09-27 核实结果 |
|--------|---------------------|
| `document.cookie` 未实现 | **假。** 已实现：getter 在 `crates/render-js/src/runtime/eval.rs:2848-2849`，setter 在 `eval.rs:3339-3342`，cookie jar 在 `crates/render-js/src/runtime/mod.rs:111-112,325-345`，回归测试 `crates/render-js/src/runtime/tests.rs:2488`。⚠️ 该 jar 只是 `name -> value` 的 map，**没有** path / domain / Secure / SameSite 作用域。 |
| "不实现 JS 引擎，只把 `<script type="application/json">` 数据岛静态渲染出来" | **已被超越。** rENDER 现在有真实 JavaScript 引擎（lexer / parser / evaluator / GC / class / Proxy / Reflect / Promise / fetch / XHR，见 `crates/render-js/`），并用 test262 做门禁（`crates/render-core/tests/test262.rs` + `tests/test262-baseline.tsv`）。数据岛静态渲染是 Python 时代的省事建议，本引擎不做，也不应回退到它。 |
| `:hover` / `:focus` / `:active` 始终匹配 | **假（方向相反）。** 动态伪类现在读显式上下文：`MatchContext.hovered/active/visited_links` 是 `HashSet<NodeId>`（`crates/render-css/src/selector.rs:285-296`），匹配在 `selector.rs:1330`。但**在线渲染路径从不填这些集合**（`crates/render-browser/src/render_worker.rs:502-504` 恒为 `HashSet::new()`，`focused: None`），所以实际结果是这些伪类**永不匹配**。详见第 5 节第 1 行。 |
| `css/selector.py`、`css/computed.py`、`layout/block.py`、`layout/flex.py`、`rendering/qt_painter.py` 等文件存在 | **假。** 这些文件属于已删除的 Python 引擎。当前对应物见 `docs/html5_scope.md` 第 12 节。 |
| `box-shadow` 只需"在 `qt_painter.py` 里调 `painter.setShadow()`" | **已过时但方向正确。** `box-shadow` 现在真的在画：`crates/render-core/src/paint/display_list.rs:1907` 起 `parse_box_shadow` 解析多层阴影并构造命令。 |
| `text-shadow` 与 `box-shadow` 同批修复 | **已发生。** `text-shadow` 已生产绘制命令（`display_list.rs:1194-1207`，多层解析 `:2435`，光栅化 `crates/render-core/src/paint/raster.rs`）。`text-decoration-*`（`:1164-1232`）与 `list-style*` marker（`:1298-1420`）也已接线 —— 这三项在旧版文档里从未被记录。 |
| `::before` / `::after` 约 80 行（cascade 生成虚拟节点 + layout 参与） | **仍未发生。** 选择器能解析（`crates/render-css/src/selector.rs:539-584`），但布局与绘制两侧都没有伪元素盒生成，见第 5 节。 |
| CSS Grid 需要"新建 `layout/grid.py` 约 300 行" | **已过时。** Grid 已实现：`crates/render-layout/src/grid.rs`（轨道/`auto-fit`/`auto-fill`/`minmax`/`repeat`）+ `crates/render-layout/src/solver/grid.rs`（放置），能力登记在 `crates/render-core/src/spec/registry.rs:14`（`css.grid-explicit-tracks`）。 |
| `position: sticky` 约 50 行、绘制时偏移即可 | **未发生。** 关键字在 `crates/render-css/src/properties.rs:758`，无任何消费者（`docs/visual_fidelity_gaps.md` S6）。 |
| `transform` 基础只需在 display list 填矩阵 | **已过时但方向正确，且已做。** `transform` / `transform-origin` 已端到端接通（`crates/render-core/src/paint/display_list.rs:764-800`，`:2179` opacity，`:2189` `fragment_stacking_context`）。 |
| `@font-face` 用 `QFontDatabase.addApplicationFont()` 注册，约 40 行 | **未发生。** `@font-face` 解析后直接丢弃：`crates/render-css/src/stylesheet.rs:558-559`；`crates/render-core` 内检索 `@font-face` 无任何命中。 |
| SVG `<img>` 靠 PyQt6 内置 `QSvgRenderer` | **已过时但能力已具备。** 现在是自研栅格化器 `crates/render-core/src/image/svg.rs`（该文件 1-18 行自述其子集：基本图形 / `path` 的 `M m L l H h V v C c S s Q q T t Z z`（`A` 弧退化为直线）/ `<g>` 变换 / `fill`+`stroke` 继承 / 尺寸按 `viewBox` 回退），入口在 `crates/render-core/src/image.rs:585-586`。 |
| `radial-gradient` 只差渲染端 20 行 | **未发生。** 情况更糟：见第 5 节"已解析未消费"表。 |
| `loading="lazy"` 需要"删一个 if" | **已过时。** 全仓库检索 `"loading"` 只有一处命中，且是 JS 侧 `loading` 属性反射（`crates/render-js/src/runtime/builtins/dom.rs:418`）；图片发现路径根本不看 `loading`，所以所有图片立即加载，**该行为已经如此**，无需修改。 |
| `document.cookie` 缺失导致 qq 页面 1364 次 `console.error` | **已过时。** 见上表。`.diag/qq/GAP_REPORT.md` §3 P1-4 已经过期。 |
| 淘宝"完全不加载"的根因是网络 30-60s 静默卡死 | **已过时。** `HANDOFF.md`「同日续」记录根因为事件循环里 `submit_with_queue_full_backoff` 的 `thread::sleep` 指数退避把 UI 线程睡死，已改为非阻塞 Deferred park + `poll_network` 重试 + 30s 一次性 stall 上报，**实测 30s 内 3 帧（此前 0 帧）**。 |

---

## 1. 这三个站点需要什么能力（本节保留旧文档的核心价值）

旧文档最有价值的部分不是实现计划，而是**"门户站真实需要什么"**。这部分结论与引擎实现无关，
保留如下，并按 2026-09-18 的实机观测补齐淘宝与京东。

### 1.1 qq.com

- 初始 HTML 含**部分静态骨架**（导航、少量新闻条目），大量内容由 JS 注入。
- 2026-09-18 实测：`.diag/qq/GAP_REPORT.md` 记录首 ~30s 接近无样式，60s 后基本正确
  （蓝色渐变头图、logo 图片、两列新闻列表、"产品推荐"侧栏、页脚链接行）。
  帧指标：`display_items` 140 → 330 → 398，`content_height` 3775.6（pre-CSS）→ 1603.6
  （post-CSS），viewport 1026，图片 38/38 全部取回并有真实尺寸。
- 因此 qq.com 需要的能力集合：
  1. **一次样式表往返**（`index.css` 数百 KB）必须完整解析并真正进入管线；
  2. **图片批量加载**（38 张，全部成功）与固有尺寸参与布局；
  3. **JS 引导**（主 bundle + `aria.js`），失败必须可定位到行/列；
  4. **渐变、圆角、阴影、表格、图片混排**的绘制；
  5. **cookie 读写**（页面登录态 helper 每轮迭代都读）；
  6. **伪元素装饰**（角标、箭头、分隔线）—— 站点 CSS 大量依赖。

### 1.2 taobao.com

2026-09-18 实测（`.diag/qq/GAP_REPORT.md` §2）：

- 静态骨架（灰色占位块、SEO 页脚段落含橙色内联链接）**渲染正确**，说明骨架的 flex/grid
  布局与内联样式通路是好的。
- 文档本身健康：`curl https://www.taobao.com/` 返回 200、94038 字节、`text/html; charset=utf-8`。
- 静态 HTML 含 14 个外链 `<script src>` + 14 个内联 script + 5 个样式表链接。
- 全部动态内容（搜索框行为、导航、商品网格）**永不出现**，`content_height` 恒等于
  viewport 1026，页面级图片请求数为 0。
- 当时三个根因：① 网络批次静默卡死；② `start_classic_scripts()` 把脚本 **fetch**
  门控在"全部样式表 resolve"之后；③ `o.alicdn.com/tbhome/tbnav/index.js`（266 KB）
  编译失败于 byte 266046（现代 ES 语法缺口）。

**注意根因 ② 是一个规范偏差，不是性能优化点**：样式表阻塞的是脚本**执行**，不是脚本
**发现与取回**。当前 `crates/render-browser/src/app.rs` 的 `start_classic_scripts()`
是否仍存在该门控，本次未能读到该函数体，**记为 ❓ 未核实**（见第 3 节 G-3）。

### 1.3 jd.com —— 共享的在线管线问题

`HANDOFF.md`「京东在线问题精确诊断」记录了一个与站点无关的引擎缺陷，对三个站点同等适用：

- 在线时序：JS 执行后渲染，`stylesheets=0`、内联样式 650 条、918 fragments；
  外链 CSS 批次应用后**再次**渲染，`stylesheets=3`、**492 fragments**、
  `content_height` 3028 —— 样式确实进了管线，**但视觉是裸文本**。
- 离线对照：保存的 SSR HTML + 同 3 个 CSS，`layout_chain_diag` 显示 `.search-m` 子树
  完美（`.form` 1008×44 红边框、`input.text` 856×40、红色 `button.button`），
  **同一引擎同一 CSS 离线全对**。
- 假设 A（内联 JS 改 DOM 破坏选择器）已被 `crates/render-core/examples/dom_dump.rs`
  **证伪**：把脚本执行后的 DOM 序列化回 HTML 再离线渲染，与原始 DOM 渲染逐像素一致。
- 剩余唯一差异：在线还成功执行了 5 个外链脚本（jquery-1.6.4、`wl.js`、`o2_ua.js`+`event.js`
  等）。下一次定位应仿 `crates/render-js/examples/bilibili_diag.rs` 写 `jd_diag.rs`
  完整回放外链脚本后再序列化 DOM 对比。
- 若回放后仍正常，则问题在浏览器侧提交链路：8 次渲染只有 3 帧 commit，大量结果被
  `drain_latest` 丢弃，存在"**带样式的渲染结果被丢弃、裸文本旧帧当最终帧**"的直接嫌疑。

这条不是京东专属问题，应按 `docs/generic-browser-todo.md` Priority 0（单一通用渲染路径）
在 `render-browser` 内定位。

---

## 2. 当前状态：逐能力核实

下表每一行的"核实结果"都带 `path:line`。未找到证据的一律标 ❓，不做推断。

| 能力 | 站点需要 | 2026-03-20 的说法 | 2026-09-27 核实结果 |
|------|---------|-------------------|---------------------|
| 一次样式表往返 | qq ✔ / tb ✔ / jd ✔ | 🟡 cascade 有 origin 元数据缺口 | ✅ 样式表解析、UA/author/inline 三源 cascade、层叠层排序均已实现。`crates/render-css/src/cascade.rs:115-241` 排 origin → layer → specificity；`@layer` 语句与块在 `crates/render-css/src/stylesheet.rs:186,217,500-528` 展开，排序测试 `cascade.rs:1018,1175,1199`。⚠️ `@supports` **条件被丢弃**（`stylesheet.rs:539-551` 的 `let _ = query;`，嵌套规则无条件应用）—— 见第 3 节 P3 之后未列的独立条目，本文按 `docs/html5_scope.md` 4.9 记录该偏差 |
| 选择器鲁棒性 | qq ✔（真实 `index.css`） | 🟡 动态伪类常态匹配 | ✅ 选择器覆盖面远超旧文档记录：属性选择器 7 种算子 + `i`/`s` 标志（`selector.rs:185-192,587-628,1190-1245`）；伪类含 `:is/:where/:not/:has/:nth-*(child|of-type|last-*)/:lang/:target/:focus-within/:placeholder-shown/:checked/:disabled/:enabled/:link/:any-link/:visited/:root/:scope/:empty`（`selector.rs:649-684`）。⚠️ `.diag/qq/GAP_REPORT.md` 记录的"index.css byte 25/53/58 `expected a selector combinator`"本次无法复现（无法运行浏览器），**记为 ❓ 未核实**。 |
| 图片加载 + 固有尺寸 | qq ✔ 38 张 | 🔵 | ✅ `crates/render-core/src/image.rs`：`picture` 源选择、`srcset`（含 `sizes` 与 `w`/`x` 描述符，`image.rs:987-1112,1148`）、格式嗅探 `image.rs:412-420`、PNG/JPEG/GIF/WebP + 自研 SVG。⚠️ AVIF 无编解码器（`ImageFormat` 枚举 `image.rs:365-371` 不含 AVIF）。 |
| 渐变 | qq ✔ 头图 | 🔵 `linear/radial-gradient` | 🟡 **只有 linear**。`linear-gradient` 解析于 `crates/render-css/src/properties.rs:1712-1720`，画于 `crates/render-core/src/paint/display_list.rs:1998`（`parse_linear_gradient`），光栅化于 `crates/render-core/src/paint/raster.rs`（`paint_linear_gradient`）。`radial-gradient()` 未进入解析器；`DisplayCommand::RadialGradient` 变体存在（`display_list.rs:286,326`）但**无任何构造点**。见第 5 节。 |
| 圆角 | qq ✔ | 🟡 两值椭圆未处理 | 🟡 已解析但退化：`border-radius: a / b` 的斜杠语法在 `display_list.rs:1817` 解析，然后对每个角用 `f32::midpoint(horizontal, vertical)`（`parse_radius_list` 在 `:1834`）**把椭圆平均成圆**。这是对 CSS Backgrounds 3 §5.5 的**已记录偏差**，不是崩溃。见第 5 节第 2 行。 |
| 阴影 | qq ✔ 卡片/弹框 | 🔵 `box-shadow`/`text-shadow` | ✅ **两者都已接线。** `box-shadow`（`display_list.rs:1907` 起 `parse_box_shadow` 解析多层阴影）、`text-shadow`（`:1194-1207` 生产命令，`:2435` 多层解析，`:2492` 包围盒）。 |
| 文本装饰 / 列表 marker | qq ✔ 链接下划线、列表圆点 | 旧文档完全没列 | ✅ `text-decoration-*`（`display_list.rs:1164-1232` 生产，`PaintPhase::TextDecoration` 在 `:342`，沿格式化祖先传播 `:1232-1296`，光栅化 `crates/render-core/src/paint/raster.rs` 按 Solid/Double/Dotted/Dashed/Wavy 分线型）；`list-style-type`/`list-style-position`（`:1298-1420` 产出 `ListMarker`，含 disc/circle/square 与有序标记文本）。⚠️ 两者都是**绘制侧所有权**：`text-decoration` 的传播是绘制侧近似而非 cascade 内传播（`display_list.rs:1232-1238` 自述中间内联的 `text-decoration-line: none` 还关不掉装饰） |
| 伪元素 `::before`/`::after` | qq ✔ 大量装饰 | 🔵 约 80 行 | 🔴 **选择器能解析，但没有盒子生成**。`selector.rs:539-584` 解析 `::before`/`::after` 与函数式伪元素；`render-layout` 与 `render-core/src/paint` 全文检索 `::before`/`::after`/`generated content` 无命中；`crates/render-browser/src/render_worker.rs:499` 把 `pseudo_element` 恒置 `None`。CSS 伪元素盒在布局与绘制两侧都不存在。见第 5 节。 |
| 文本轴（粗体/斜体/字体族） | 三个站点全需要 | 🟡 "font 基础属性" | 🔴 **最致命的一条，且旧文档完全没有记录。** `crates/render-layout/src/solver/mod.rs:33-36` 的 `TextStyle` 只有 `font_size` + `line_height`；`crates/render-browser/src/font_backend.rs:33-43` 每个候选组只加载**一个**字体就 break，`:57-63` 只按字形覆盖选字体。粗体/斜体物理上不可能，`font-family` 被忽略。证据与排序见 `docs/visual_fidelity_gaps.md` S1。 |
| UA 样式表 | 三个站点全需要 | 🟡 | ✅ **已按 WHATWG HTML "Rendering" 章节重写**（`crates/render-core/src/document.rs:70-211`，142 行，逐节标注 §15.3.1–§15.3.12 与 §15.5.5）。含 `a:link`/`a:visited` 颜色与下划线、`h1`-`h6` 各自的 `font-size`+`font-weight`+边距、`b,strong{font-weight:bolder}`、`cite,dfn,em,i,var,q,address{font-style:italic}`、`code,kbd,samp,tt` 等宽族、`big/small/sub/sup`、`ins,u` 下划线与 `del,s,strike` 删除线、`ol/ul` 四级 `list-style-type`、`mark` 黄底、`dialog:not([open])`、`blockquote`/`figure` 缩进、`table{border-spacing:2px}` + `td,th{padding:1px}` + `th{font-weight:bold}`、`hr`、`fieldset`/`legend`、`details>summary:first-of-type`。HTML 展现属性（`bgcolor`/`width`/`align`/`cellpadding`…）已映射为 UA-origin 声明（`document.rs:213-` 起）。🔴 **但表内所有字体轴声明无消费者**（见上一行）—— 标题字号变了，字重与字体族仍与正文相同。🔴 quirks 模式仍未实现（`document.rs:847-855`）。表注释 `document.rs:47-69` 把每一处适配显式标为能力缺口 |
| JS 引擎 | qq 主 bundle / tb React | 🔵 "从零实现 JS 是量级相当的工程，约 600 行 Phase JS-1" | ✅ **已远超旧计划的规模。** `crates/render-js/`：lexer / parser（完整 class 语法、私有名、`??`/`??=`/`&&=`/`||=`、XID 标识符）/ runtime（evaluator、GC、`runtime/class.rs` 完整类语义、Proxy/Reflect、Promise、Iterator helpers、typed arrays）/ 按 Web API 域分文件的 builtins。test262 门禁基线 **30,808 / 98,096 变体通过（31.41%），264 桶，crash 22，timeout 20**（`crates/render-core/tests/test262-baseline.tsv`，合计由 `tests/test262.rs:234` 的 `enforce_baseline` 把关）。❓ 淘宝 `tbnav/index.js` byte 266046 的编译失败本次无法复现，**未核实**。 |
| 事件循环 / 微任务 | 三个站点全需要 | 🔵 "已有 `js/event_loop.py` 骨架" | ✅ `crates/render-core/src/event_loop.rs`：`TaskSource` 分类（`:23`）、task queue（`:344`）、microtask 队列与容量上限（`:70-72,361-369`）、`perform_microtask_checkpoint`（`:480-508`）、每 task 后的 rendering opportunity（`:443-476`）。`setTimeout/setInterval/clearTimeout/clearInterval/requestAnimationFrame/cancelAnimationFrame` 装在 `crates/render-js/src/value.rs:2633-2652`。 |
| DOM API | 三个站点全需要 | 🟡 | ✅ 查询、遍历、属性反射、`classList`、`dataset`（`crates/render-js/src/runtime/builtins/dom.rs:386-401,775-820`）、`innerHTML`/`textContent`、`getComputedStyle`（`value.rs:1399`，实现 `dom.rs:146`）、`MutationObserver` 与 `IntersectionObserver`（`crates/render-js/src/runtime/builtins/observers.rs`）、`Proxy`/`Reflect`（`builtins/proxy.rs`，`value.rs:4229,4280`）、`URL`/`URLSearchParams`（`builtins/url.rs`）。 |
| 网络（fetch / XHR） | 三个站点全需要 | ⛔（旧文档完全没列） | ✅ `crates/render-js/src/runtime/builtins/fetch.rs`（`fetch`、`Response`、经典 `XMLHttpRequest`）。⚠️ `crates/render-core/src/spec/registry.rs:266-273` 仍把 `fetch.runtime` 登记为 `SupportStatus::Missing` 且 `tests: &[]` —— 登记与实现不一致，见 `docs/html5_gap_matrix.md` 的 Fetch/XHR 行 |
| 事件派发 | 点击/登录/搜索 | 🔵 | 🟡 冒泡 ✅（`crates/render-js/src/runtime/builtins/events.rs:163-168,236,283`），`preventDefault` 驱动默认动作 ✅（`crates/render-browser/src/app.rs:2048-2069`）。🔴 `addEventListener` 的**第三个参数被完全忽略**（`events.rs:106-127` 只读 type + callback）—— `capture` / `once` / `passive` 一律无效。 |
| 表单提交 | jd 登录 / 站点搜索框 | 🔵 PLANNED M2 | 🟡 GET 导航 ✅：可取消的 `submit` 事件（`app.rs:2048-2069`）、提交计划 `crates/render-core/src/interaction.rs:783-827`（含 `form` 属性关联、disabled 排除、成功控件收集）、`app.rs:3009-3017` 导航。POST：`crates/render-net/src/transport.rs:160,274,946` 传输层支持带体 POST，但 **`app.rs:3009` 过滤掉非 GET 提交**，浏览器侧不导航。 |
| 资源加载顺序 | tb 卡死根因 | — | 🟡 有界并发与 per-origin 上限（`crates/render-net/src/batch.rs:34,122-172`）、队列满时 Deferred park + 重试 + 30s 一次性 stall 上报（`HANDOFF.md`「同日续」）。`NetworkWorker` 保留一个逻辑 CPU 给事件循环（`crates/render-net/src/worker.rs:209`）。 |
| HTTP 缓存 | 通用 | — | ✅ 私有内存 LRU（32 MiB）+ `ETag`/`Last-Modified` 条件重验证；有界 512 MiB 磁盘 store + 代际安全清理（`crates/render-browser/src/cache.rs`、`cache/disk.rs`、`cache/payload.rs`）。 |
| 视频解码 | — | ⛔ | 🔴 只有 poster + 状态机（`crates/render-js/src/video/present.rs`、`mod.rs:40-41,58` 的 `PlaceholderDecoder` 恒返回 `DecoderUnavailable`）。demuxer/AVC 骨架在 `video/demuxer.rs`、`video/avc.rs`，像素解码未写。 |
| Canvas | — | ⛔ | ⛔ 无 2D 上下文。`DisplayCommand::Canvas` 变体存在（`display_list.rs:327`）但无构造点。 |

---

## 3. 仍然缺失（按实现难度 / 收益比排序，与旧文档的排序法一致）

排序法保留：CSS / 布局层收益最确定、实现最干净；JS 与网络层放最后且要做就做正确。
所有条目的名称都是**通用能力**，不含站点名。

### P1 — 低难度、立刻提升正确性

| # | 能力 | 为什么优先 | 证据 |
|---|------|-----------|------|
| 1 | **字体轴（weight / style / family）** | **现在是第一优先。** UA 样式表已按 HTML §15.3 写对 `b,strong{font-weight:bolder}`、`em,i,cite,dfn,var{font-style:italic}`、`code,kbd,samp,tt{font-family:monospace}`、`h1..h6{font-weight:bold}` + 各自 `font-size`，但这些声明**全部无消费者** ⇒ 标题与正文只有字号不同，字重与字体族完全相同。`TextStyle` 要加三个字段，`TextMeasurer`/`TextShaper`/`TextPainter` 三个 trait 透传，backend 改为按 `(family, weight, style)` 选字面。跨 `render-layout` + `render-browser`，须排在持有这两个 crate 的工作之后 | `docs/visual_fidelity_gaps.md` S1；`crates/render-layout/src/solver/mod.rs:33-36`；`crates/render-browser/src/font_backend.rs:33-43,57-63`；UA 表 `crates/render-core/src/document.rs:91,92,104,105,106,130-136` 与其自述 `document.rs:56-62` |
| 2 | **`::before`/`::after` 生成内容** | qq 这类门户的装饰几乎全靠伪元素；选择器已能解析，只差 cascade→layout 的盒子生成 | `selector.rs:539-584`；`render_worker.rs:499` |
| 3 | **quirks 模式** | 无 doctype 的老页面现在按 standards-mode 盒模型排，必然错；UA 表注释也声明"this sheet is the no-quirks rendering" | `crates/render-core/src/document.rs:65-67,847-855` |
| 4 | **动态伪类状态接线** | `:hover/:focus/:active/:visited` 与 `:target` 现在永不匹配，`:focus` 环与 hover 反馈在页面上完全不存在；浏览器壳内建新标签页的 `.favorite-link:hover` 规则（`crates/render-browser/src/home.rs:144,262`）也因此从不生效。⚠️ 注意 UA 表已写 `a:visited{color:#551a8b}`（`document.rs:117`），它同样永不生效 | `render_worker.rs:495-508` 恒置空；`selector.rs:1330` |
| 5 | **elliptical `border-radius`** | 斜杠语法已解析但被 `f32::midpoint` 平均成圆，胶囊形按钮与头像被画成正圆 | `display_list.rs:1817,1834` |

### P2 — 中等难度、高收益

| # | 能力 | 为什么优先 | 证据 |
|---|------|-----------|------|
| 6 | **内联 SVG 栅格化** | **解析侧已完成**：`crates/render-html/src/tree_builder.rs` 现在有完整 foreign content（`:130-183` 判定、`:556-567` `math`/`svg` 起始标签、`:1237-1278` 积分点、`:1292-1400` foreign content 规则、`:1600-1860` 命名空间/属性调整表）。**渲染侧为零**：`render-layout` 与 `render-core/src/paint` 检索 `Namespace::Svg` 无命中 ⇒ 全站图标消失。做法是把 svg 子树序列化回 SVG 文本喂给**已有的** `crates/render-core/src/image/svg.rs`，注册为 image resource 走替换元素尺寸，约百行且不需新渲染代码 | `crates/render-html/src/tree_builder.rs:1290`；`crates/render-core/src/image/svg.rs:1-18` |
| 7 | **`@font-face` 消费 + Web 字体** | 几乎所有中文站自托管普惠体/思源黑体/HarmonyOS Sans；解析后丢弃导致全站无 webfont。与字体轴是同一条链：先有 `font-family` 消费，`@font-face` 才有意义 | `crates/render-css/src/stylesheet.rs:558-559` |
| 8 | **`radial-gradient` 解析 + 生产者** | 命令变体和光栅化分支已就位，缺解析与构造两步 | `display_list.rs:286,326`；`crates/render-core/src/paint/raster.rs` 的 RadialGradient 臂 |
| 9 | **真正的堆叠上下文 + z-index** | 现在只有 `transform`/`opacity` 会创建堆叠上下文；z-index 仅在 block 容器子级排序，且排序键是"自身与全部后代的最大值"这一启发式 | `display_list.rs:2189`；`crates/render-layout/src/solver/mod.rs:399-426`；`solver/block.rs:744-763` |
| 10 | **margin collapsing** | 相邻兄弟、父子、穿透空盒的折叠全缺；`solver/block.rs:258-259` 各自解析上下边距后直接相加（`:411`），没有 `max(+, −)` 步骤 | `crates/render-layout/src/solver/block.rs:258-259,411` |
| 11 | **`position: sticky`** | 吸顶/吸底导航是门户标配 | `crates/render-css/src/properties.rs:758`（定义关键字，无消费者） |
| 12 | **RTL / `direction`** | UA 表因布局求解器只消费物理 longhand 而全部物理化书写，表注释声明这张表是 LTR 的（`crates/render-core/src/document.rs:50-55`）⇒ 所有中文/阿拉伯站点排版方向错误 | `crates/render-css/src/computed.rs:73`（仅初始值）；`crates/render-layout/src/geometry.rs:88-110`（有 `Direction` 类型但无 CSS 读入通路） |

### P3 — 中等难度、中等收益

| # | 能力 | 证据 |
|---|------|------|
| 13 | `@keyframes` / `transition` / `animation`（需要一个动画时钟） | `crates/render-css/src/stylesheet.rs:552-557`（`KeyframesBlock` 丢弃） |
| 14 | `letter-spacing` / `text-indent` / `text-transform` / `text-overflow` / `word-break` / `overflow-wrap` | `docs/visual_fidelity_gaps.md` S3/S6（`text-decoration*`/`text-shadow`/`list-style*` 已接线） |
| 15 | `text-decoration` 传播下沉到 `render-css`（当前是绘制侧近似） | `display_list.rs:1232-1238` |
| 16 | `addEventListener` 的 `capture` / `once` / `passive` | `crates/render-js/src/runtime/builtins/events.rs:106-127` |
| 17 | POST 表单导航（传输层已有，壳未接） | `crates/render-browser/src/app.rs:3009` |
| 18 | `text-align: justify`（当前解析后按 start 渲染） | `crates/render-layout/src/solver/inline.rs:446-467` |
| 19 | `object-position`（`object-fit` 五个关键字已实现） | `display_list.rs:1497,1621`；`object-position` 全仓库无命中 |
| 20 | AVIF 解码 | `crates/render-core/src/image.rs:365-371`（`ImageFormat` 无 AVIF） |
| 21 | `filter` / `clip-path` / `backdrop-filter` / `mask-image` / `mix-blend-mode` | `docs/visual_fidelity_gaps.md` S3（仍零消费者） |

### P4 — 阻塞级（会让上面几条的实机验证做不了）

| # | 能力 | 说明 |
|---|------|------|
| 22 | **在线管线"有样式却裸文本"** | 见 1.3 节。同一引擎同一 CSS 离线全对，在线裸文本；已排除内联 JS 改 DOM，剩余变量是外链脚本回放与浏览器侧 commit 竞态。必须先定位，否则任何 CSS 修复都无法用实机验证 |
| 23 | **样式表不得门控脚本 fetch** | 规范上样式表阻塞脚本**执行**，不阻塞脚本**发现/取回**。`.diag/qq/GAP_REPORT.md` §2.2 记录 `crates/render-browser/src/app.rs` 的 `start_classic_scripts()` 曾以 `!page.styles_resolved` 提前 `break`。本次未能读到该函数体，**❓ 未核实**；请以当前源码为准 |
| 24 | **网络可观测性 + 按地址回退 + 批次有界** | `.diag/qq/GAP_REPORT.md` §2.1：引擎对 g.alicdn.com 批次 30-60s 甚至永不返回，**期间零日志**（无重试、无错误、无每请求计时），而 curl 同链路瞬时。`HANDOFF.md` 已把该工作派给 NET（`render-net/**`） |
| 25 | **JS 引擎 `require_object` 原始值 ToObject 装箱** | qq 主 bundle 报 `Throw: TypeError: getProto: not an object at line 206, column 825`。⚠️ 该错误字符串已不在 `crates/render-js/src/runtime/eval.rs` 中（本次检索无命中），`HANDOFF.md` 也把修复派给了 JS（`render-js/**`）。**修复是否已闭环未经实机验证，❓ 未核实** |

### 明确不做（⛔）

- 站点特判、站点数据源替换、借用 Chromium/Edge 截图当页面 —— `docs/generic-browser-todo.md` 明文禁止。
- `<canvas>` 2D 上下文、WebGL/WebGPU、Service Worker、IndexedDB。
- `multipart/form-data`（无文件上传能力）。
- 复杂脚本整形（阿拉伯/印度系/泰文 cluster）**不是** ⛔：它按 `docs/html5_scope.md` 4.5 记为
  🔵 PLANNED（当前整形是自研的，缺口是真实的，与任何外部库无关）。

---

## 4. 汇总：实施路线

保留旧文档"按难度/收益排序"的收尾形式，但换成当前真实的内容。

```
P1（当前最高优先：UA 表已写对规则，但字重/字体族无消费者）
  ├── 字体轴 weight/style/family            solver/mod.rs:33 + font_backend.rs:33-63
  │                                          （跨 render-layout + render-browser）
  ├── ::before/::after 生成盒                selector.rs:539-584（解析已在，缺 layout）
  ├── quirks 模式                            document.rs:847-855
  ├── 动态伪类状态接线                       render_worker.rs:495-508
  │                                          （含 a:visited，UA 表已写但永不生效）
  └── 椭圆圆角（不取中点）                    display_list.rs:1817,1834

P2（需要文件归属释放 / 新代码）
  ├── 内联 SVG 栅格化（复用 image/svg.rs）     tree_builder.rs 已就绪，paint 侧为零
  ├── @font-face 消费                        stylesheet.rs:558-559（依赖字体轴先落地）
  ├── RTL / direction 消费                   computed.rs:73 + geometry.rs:88-110
  ├── radial-gradient 解析 + 生产者           display_list.rs:286,326
  ├── 堆叠上下文 + z-index                   display_list.rs:2189 + solver/mod.rs:399
  ├── margin collapsing                       solver/block.rs:258-259,411
  └── position: sticky                       properties.rs:758

P3（局部补齐）
  ├── 动画时钟（@keyframes/transition）        stylesheet.rs:552-557
  ├── 文本排版 letter-spacing/text-indent/text-transform/text-overflow/
  │   word-break/overflow-wrap
  ├── text-decoration 传播下沉 render-css     display_list.rs:1232-1238
  ├── addEventListener options                events.rs:106-127
  ├── POST 表单导航                           app.rs:3009
  ├── filter/clip-path/mask-*/blend-mode
  ├── object-position / AVIF
  └── text-align: justify                     inline.rs:446-467

P4（先做，否则 P1-P3 无法实机验收）
  ├── 在线"有样式却裸文本"定位与修复           HANDOFF.md 京东节
  ├── 脚本 fetch 不被样式表完成门控            app.rs start_classic_scripts（❓ 待核实）
  ├── 网络可观测性 + 每地址回退 + 批次有界     render-net/{batch,worker,diagnostics}.rs
  └── require_object ToObject 装箱闭环验证     render-js（❓ 待实机复验）
```

> ⚠️ 2026-09-27 当日有并行 agent 正在 `render-core` 内作业，UA 样式表（`document.rs:70-211`）、
> `text-decoration`（`display_list.rs:1164-1296`）、`text-shadow`（`:1194-1207`）与
> list marker（`:1298-1420`）正是在本文写作期间落地的。**上面的行号是本次快照，
> 复用前请重新检索。** 字体轴缺口未受影响：`crates/render-layout/src/solver/mod.rs:33-36`
> 仍是两字段的 `TextStyle`。

---

## 5. 已实现但有已知问题

**本表逐行重新核实，2026-03-20 的原始判断不得直接沿用。** 状态列含义：
`仍成立` = 缺陷在今天的代码里仍然存在；`已修复` = 今天的代码里已不成立；
`未核实` = 本次没有找到可引用的证据。

| 缺陷 | 旧位置（Python） | 状态 | 当前位置与证据 | 影响 |
|------|------------------|------|----------------|------|
| `:hover/:focus/:active` 动态伪类 | `css/selector.py` | **已修复，但换了一个缺陷** | 旧缺陷（无条件 `True`）不再存在：现在读 `MatchContext`（`crates/render-css/src/selector.rs:285-296`，匹配 `:1330`）。新缺陷是在线路径从不填状态（`crates/render-browser/src/render_worker.rs:502-504` 恒 `HashSet::new()`、`:500` `focused: None`），于是这些伪类**永不匹配** | hover 背景、`:focus` 焦点环、`:active` 按下态全部不出现；内建新标签页自身的 `.favorite-link:hover` 规则（`home.rs:144,262`）也从不生效 |
| `border-radius` 两值（椭圆） | `css/computed.py` | **仍成立（程度减轻）** | 斜杠语法已被解析（`crates/render-core/src/paint/display_list.rs:1817`），但 `parse_corner_radii` 用 `f32::midpoint` 把水平/垂直半径平均成一个标量（`parse_radius_list` 在 `:1834`），椭圆退化为圆 | 真实站点的胶囊形/椭圆圆角被画成正圆；不是崩溃，是静默偏差。按 CSS Backgrounds 3 §5.5 记录为偏差 |
| z-index stacking context 不完整 | `layout/block.py` | **仍成立（旧文档低估了它）** | 堆叠上下文只由 `transform`/`opacity` 创建（`display_list.rs:2189` `fragment_stacking_context`、`:2179` 取 opacity）。z-index 以裸文本读取（`crates/render-layout/src/solver/mod.rs:406-413`），排序发生在 block 容器子级（`solver/block.rs:763`，稳定排序保层内源序），且排序键取"自身与所有后代的最大值"（`solver/mod.rs:414-425`）这一启发式。绘制层无 z-index 概念；flex/grid 子项不参与排序；`z-index` 自身不创建堆叠上下文 | 弹出层/遮罩/轮播指示器层级错乱；`position:relative` + `z-index` 的局部抬高会连带抬高整个后代的层号 |
| margin collapsing 不完整 | `layout/block.py` | **仍成立（实为完全未实现）** | 检索 `render-layout` 无任何折叠实现：`solver/block.rs:258-259` 分别解析上下边距，`:411` 直接相加 | 段落/列表间距偏差，且随内容长度累积 |
| `flex-basis: 0` 与 `auto` 语义差异 | `layout/flex.py` | **已修复** | 三态已分开：`FlexBasis::LengthPercentage` / `Auto` / `Content`（`crates/render-layout/src/solver/flex.rs:751-775`），`flex_basis_is_auto` 单独判定（`:974-979`），百分比 basis 对不定主轴按 §7.2.2 当 `content`（`:753-756`） | — |

### 已解析但无消费者（同一类缺陷，2026-09-27 记录）

`docs/generic-browser-todo.md` 规定"解析了但没人消费的属性是缺陷，且有位置"。
⚠️ **本次快照中这一类正在被逐个消灭** —— 写本文时（2026-09-27），
`text-decoration*` / `text-shadow` / `list-style*` 三项已被接线（见第 3 节 P1 与第 2 节表格），
它们此前正是"有类型有光栅化、无生产者"的同一形态。下面是**仍然存在**的：

| 项 | 已有 | 缺 | 证据 |
|----|------|----|------|
| `radial-gradient()` | `RadialGradient` 结构与命令变体（`display_list.rs:286,326`）、光栅化分支（`crates/render-core/src/paint/raster.rs`）、诊断标签（`crates/render-browser/src/diagnostics.rs:44`） | 解析器不认（`crates/render-css/src/properties.rs:1712-1720` 只处理 `linear-gradient`），且无构造点 | `crates/render-core/src/paint/` |
| `<canvas>` | `DisplayCommand::Canvas` 变体（`display_list.rs:327`）、诊断标签（`render-browser/src/render_worker.rs` 的 `Canvas` 臂） | 无构造点；且无 2D 上下文（⛔ 非目标） | 同上 |
| UA 表的 `font-weight` / `font-style` / `font-family` | 规则已按 HTML §15.3 正确写入（`crates/render-core/src/document.rs:91,92,104,105,106,130-136`），值也正确到达 computed style（`crates/render-css/src/cascade.rs:833-838`） | **无消费者**：`TextStyle` 无字段可接（`crates/render-layout/src/solver/mod.rs:33-36`）、字体后端不按字重/斜体选面（`crates/render-browser/src/font_backend.rs:33-43,57-63`）。UA 表自己的注释已声明这一点（`document.rs:56-62`） | 见第 3 节 P1-1 |

> ⚠️ 本表是 2026-09-27 的快照。当日有并行 agent 在 `render-core`、`render-layout`、
> `render-html`、`render-js`、`render-css` 内作业（见 `HANDOFF.md` 的 agent 归属表），
> 上述项目可能正在被接线。**复用本表前请重新检索，不要依赖行号。**

---

## 6. 本文与其他文档的关系

- 能力缺口与排序的权威来源是 `docs/visual_fidelity_gaps.md`；本文只补充"门户站真实需要什么"
  与站点侧的实机观测，不重复推导 file:line 证据。
  ⚠️ **注意**：`docs/visual_fidelity_gaps.md` 的 S2（UA 样式表 28 行）与 S3（12 个属性零消费者，
  含 `text-decoration*`/`text-shadow`/`list-style*`）在本文件写作期间已由并行工作落地，
  该证据文件的对应条目现已落后；S5（内联 SVG 解析缺失）同样已落地（foreign content 已在
  `crates/render-html/src/tree_builder.rs`）。**S1（字体轴）仍然成立**，且因为 UA 表现在
  写对了规则而更刺眼。本文以代码为准，不以该证据文件为准。
- 全部能力簇的登记状态在 `crates/render-core/src/spec/registry.rs`（20 条，全部 `Partial`，
  外加 `fetch.runtime` 标 `Missing`）。该登记表尚未覆盖动画、`@font-face`、伪元素、表格、
  内联 SVG、视频解码、表单等缺口，见 `docs/visual_fidelity_gaps.md` S9。
- 范围与标签定义在 `docs/html5_scope.md`；测试与验收基线在 `docs/html5_gap_matrix.md`。
