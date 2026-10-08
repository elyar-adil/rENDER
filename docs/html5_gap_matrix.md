# rENDER Web 平台能力矩阵

本文档是引擎实现顺序和验收门槛，不是功能宣传页。状态只能由可重复的测试结果推进：

- `Missing`：没有可用实现；
- `Partial`：存在实现，但规范范围或外部一致性测试仍有已知缺口；
- `Conformant`：固定版本的适用外部测试全部通过，且没有已知偏差。

真实网站只用于阶段性集成和视觉验收。网站问题必须缩减成规范测试或最小 fixture，禁止域名特判
（由 `tools/check_site_neutrality.py` 在 CI 中机械检查，见 `.github/workflows/rust.yml:53-54`）。

> **本次重写：2026-09-27。** 上一版（2026 年初，Python + PyQt6 原型期）把 test262
> 通过数记为 11,628（8.1%），并把 WPT 入口写成 `tools/run-wpt-reftests.py`。
> 两者都已被超越，本次按当前仓库实测数据更正，并把三行被低估的能力簇
> （CSS Fonts/Text、Painting/Stacking、Replaced elements）改为与
> `docs/visual_fidelity_gaps.md` 一致的口径。代码状态为提交 `2bd8e8c` 加当日在途改动。
>
> ⚠️ **快照与漂移**：写作期间有并行 agent 落地了 HTML §15 UA 样式表
> （`crates/render-core/src/document.rs:70-211`）与 `text-decoration` / `text-shadow` /
> list marker 的绘制接线（`crates/render-core/src/paint/display_list.rs:1164-1420`）。
> 本文已按落地后的状态书写；`docs/visual_fidelity_gaps.md` 的 S2、S3、S5 已落后于代码，
> S1（字体轴）仍然成立。

---

## 当前基线

| 测试层 | 当前状态 | 下一道门槛 |
| --- | --- | --- |
| Rust 单元/集成测试 | `cargo test --workspace`；测试文件为 `crates/render-core/tests/{dynamic_dom_render,layout_paint_regressions,layout_positioning,paint_images,js_conformance}.rs`、`crates/render-net/tests/{connection_reuse,connect_budget,local_transport,proxy_transport}.rs`，以及各 crate 的 `#[cfg(test)]` 模块。⚠️ 上一版写的 "`render-core` 249 项通过" **无法核实**，本次不引用具体用例数 | 每个规范修复必须先有最小回归测试 |
| test262 | 固定 revision（由 `tools/fetch-test262.sh` 取入 `third_party/test262`），264 个桶，**pass 30,808 / 98,096 变体（31.41%）**、fail 60,598、timeout 20、crash 22。基线文件 `crates/render-core/tests/test262-baseline.tsv`（表头自带口径说明），门禁 `crates/render-core/tests/test262.rs:234-274` 语义为"桶内 pass 不得回退"，不是"全绿"。`RENDER_TEST262_UPDATE_BASELINE=1` 重建基线 | 按失败簇提升；已通过集合不得回退；crash/timeout 归零 |
| WPT | 官方固定 revision `c7fdee80f3f17b4e9813964916afdfd57ace863f`（`crates/render-core/tests/wpt_reftests.rs:21`），外部 checkout 由 `tools/fetch-wpt.ps1` 取入，通过 `RENDER_WPT_ROOT` + `RENDER_WPT_MANIFEST` 配置。**入口是 Rust 测试 `cargo test -p render-core --test wpt_reftests`，不是 `tools/run-wpt-reftests.py` —— 该脚本不存在。** 测试默认 `#[ignore]`（`wpt_reftests.rs:88-90`），CI 用 `RENDER_WPT_REFTESTS=1` 打开并允许 skip（`.github/workflows/rust.yml:67-88`），skip 不得记为 pass。已知限制：`@import` 与 `url()` 资源在该 runner 内标 unsupported（`wpt_reftests.rs:356-358`） | 跑通完整 batch 拿到第一批 `WPT_SUMMARY` 数字；再单独接入 testharness / navigation |
| 浏览器视觉对比 | **没有仓库内的 Chromium 对比工具。** 现状是"照片对比法"：`RENDER_DUMP_FRAME` 让 rENDER 自己写 PPM，`tools/ppm2png.py` 转 PNG，参照截图从仓库外取得后**人工/视觉比对**。fixture 在 `example/`（`index.html`、`hao123.html`、`hao123_2003.html`、`hn.html`）与 `.diag/`。⚠️ 上一版写的 `tests/browser_visual_regression.py` 不存在 | 每个布局簇提供几何断言（比截图 diff 更可自动化） |
| 渲染性能 | release 构建的 deterministic `render-perf`（`crates/render-browser/src/bin/render-perf.rs`），输出 parse / first render / first_visible / scroll 的 JSON 分布。CI：`.github/workflows/perf.yml`，push+PR 跑 `--fixture generated` smoke（3 迭代），每周一与 `workflow_dispatch` 跑 `--fixture all` 全量（20 迭代），均上传 JSON artifact | 在同一测试机记录 JSON 基线，比较 `first_visible` p95，并以至少 30% 相对下降作为阶段目标；未有基线不得宣称优化完成。已知瓶颈：滚动每帧重跑完整管线（`HANDOFF.md` 2026-09-26 实测 1280×720 / 5380 fragments 页面 scroll 中位 116ms/帧 ≈ 8.6fps），retained display-list 增量渲染未做 |
| HTTP 缓存基础设施 | 私有内存 LRU（32 MiB，只收显式 fresh 的匿名响应）、过期 `ETag/Last-Modified` 条件重验证；有界 512 MiB 磁盘 store / I/O worker、校验和原子记录、代际安全清理（`crates/render-browser/src/cache.rs`、`cache/disk.rs`、`cache/payload.rs`） | 将磁盘 read-through/write-back 接入资源管线，并扩展 Vary、启发式 freshness 与 Fetch 语义 |
| 能力自登记 | `crates/render-core/src/spec/registry.rs`：20 条 `FeatureDefinition` **全部** `SupportStatus::Partial`，外加 `fetch.runtime` 标 `Missing` 且 `tests: &[]` | 登记表必须与实现一致，且补登动画/transition、`@font-face`、伪元素、表格、内联 SVG、视频解码、表单等条目（`docs/visual_fidelity_gaps.md` S9） |

**关于 pass 率的表述纪律**：test262 当前 31.41%，可以描述为"ES2020 前后的语言核心大部分可用"，
**不能**描述为"JavaScript 已完成"。WPT reftest 在跑出 `WPT_SUMMARY` 数字之前，
任何 CSS/DOM 子系统都不得标为 `Conformant`（`registry.rs` 的 20 条 `Partial` 与此一致）。

---

## P0：首屏正确性地基

| 能力簇 | 状态 | 已有能力 | 主要缺口 | 验收测试 |
| --- | --- | --- | --- | --- |
| CSS Syntax | Partial | 规则、声明、`@media` / `@layer` / `@supports` / `@keyframes` / `@font-face` 的容错解析（`crates/render-css/src/stylesheet.rs:160-262`），`@keyframes`/`@font-face` 丢弃时不报错（`:552-559`），未知 at-rule 记 `capability_diagnostic`（`:561-566`） | token/escape 细节、错误恢复、**`@import` 未实现**（走 `IgnoredAtRule`）、**`@supports` 条件被丢弃**（`:539-551` 的 `let _ = query;` ⇒ 条件不成立时嵌套规则也会生效）、`@page`/`@property`/`@scope`/`@counter-style` | WPT `css/css-syntax/` |
| Selectors/Cascade | Partial | 7 种属性选择器算子 + `i`/`s` 标志（`crates/render-css/src/selector.rs:185-192,587-628`）；31 个伪类，含 `:is/:where/:not/:has/:nth-last-*(child|of-type)/:lang/:target/:focus-within/:placeholder-shown`（`:649-684`）；UA/User/Author/inline origin + `!important` 反转（`cascade.rs:136,926-940`）；`@layer` 完整排序与 `revert-layer`（`stylesheet.rs:186,217,500-528`；`cascade.rs:877-924,1175,1199`）；origin/layer/importance 元数据保留（`cascade.rs:66-68,135-165`） | **动态伪类状态从不接线**（`crates/render-browser/src/render_worker.rs:495-508` 恒置 `focused/target = None`、`hovered/active/visited_links = 空集`）⇒ `:hover/:focus/:active/:visited/:target` 永不匹配；**伪元素可解析可匹配但无盒子生成**（`selector.rs:539-584` 有解析，`render-layout`/`paint` 无生成，`render_worker.rs:499` 恒 `None`）；CSS-wide keywords 逐项行为未逐一核实 | WPT `css/selectors/`、`css/css-cascade/` |
| Values/Units | Partial | `calc/min/max/clamp/fit-content(<lp>)`（`crates/render-css/src/properties.rs:533-585,2891-2958`）；`vmin/vmax` 与 `sv*/lv*/dv*/cq*` 全族（`:498-523`）；`currentColor`（`:988,1671`）；颜色 `rgb/hsl/hex/named`（`:1671-1788`）；`var(--x, fallback)` 含无效自定义属性记录与 4096 上限（`computed.rs:604,745,830-860,168-178`） | `ch`/`ex`/绝对单位；`color()`/`lab()`/`lch()`/`oklab()`/`oklch()`；`env()`；`attr()`；`@container` 规则层（`cq*` 单位已能求解但无容器查询） | WPT `css/css-values/`、`css/css-color/` |
| Fonts/Text/Line boxes | Partial | 文本测量与断行、`font-size`（`crates/render-layout/src/solver/inline.rs:575`、`solver/resolve.rs:344`）、`line-height`（`inline.rs:580`）、`white-space`（`inline.rs:544,612`）、`text-align: left/right/center`（`inline.rs:446-467`）、`vertical-align`（`solver/table.rs:691`）、表格 baseline、**`text-decoration-*`（含 Solid/Double/Dotted/Dashed/Wavy）**（生产 `crates/render-core/src/paint/display_list.rs:1164-1232`、沿格式化祖先传播 `:1232-1296`、`PaintPhase::TextDecoration` `:342`、光栅化 `paint/raster.rs:797-840`）、**`text-shadow`**（生产 `display_list.rs:1194-1207`、解析 `:2435-2450`、包围盒 `:2492`）、**`list-style-type`/`list-style-position` → list marker**（`display_list.rs:1298-1420`，光栅化 `raster.rs:953-1015`，含 disc/circle/square 与有序标记文本） | 🔴 **字体轴结构性缺失（唯一仍致命的）**：`TextStyle` 只有 `font_size` + `line_height`（`crates/render-layout/src/solver/mod.rs:33-36`），`font-weight`/`font-style`/`font-family` 在 `crates/render-css/src/cascade.rs:833-838` 写进 text style 后**无处可去**；`crates/render-browser/src/font_backend.rs:33-43` 每个候选组只加载一个字体就 `break`，`:57-63` 只按字形覆盖选字体 ⇒ **粗体、斜体物理上不可能，`font-family` 被忽略**。⚠️ 这条现在更刺眼：新的 UA 样式表**已经**按 HTML §15.3.3/§15.3.4/§15.3.6 写入了 `b,strong{font-weight:bolder}`、`cite,dfn,em,i,var{font-style:italic}`、`code,kbd,samp,tt{font-family:monospace}`、`h1..h6{font-weight:bold}` + 各自的 `font-size`，而这些声明**全部无消费者** —— 规则正确但一个字重变化都不会发生。UA 样式表自己的文档注释（`crates/render-core/src/document.rs:56-62`）已把这一点标为引擎适配而非偏好。🟡 `text-decoration` 的传播是绘制侧近似（`display_list.rs:1232-1238` 自述）：沿格式化祖先取第一个指定该线的祖先，中间内联元素上的 `text-decoration-line: none` 还不能关掉装饰，正确修法是把传播移进 `render-css`。🟡 `text-align: justify` 解析后按 start 渲染（`inline.rs:462-467`）。🔴 仍有 10 个 paint/text 属性零消费者（`docs/visual_fidelity_gaps.md` S3 中未被本轮接线的部分）：`letter-spacing`、`text-indent`、`text-transform`、`text-overflow`、`filter`、`clip-path`、`backdrop-filter`、`mask-image`、`mix-blend-mode`（`text-decoration*`/`text-shadow`/`list-style*` 已接线）。🔴 `@font-face` 丢弃（`stylesheet.rs:558-559`）⇒ **全站无 webfont**。🔴 `direction: rtl` 无消费（仅初始值 `computed.rs:73`） | WPT `css/css-fonts/`、`css/css-text/`、CSS2 line box |
| Block/Inline formatting | Partial | 基础 block/inline、匿名块包裹（`crates/render-layout/src/tree.rs:782`）、`display: contents`（`:814-827`）、float + 清除、`overflow:hidden` 自动包含浮动（`crates/render-core/tests/layout_positioning.rs:119`）、`aspect-ratio`（`solver/block.rs:454`）、`position: relative/absolute/fixed`（`solver/block.rs:765-782`） | 🔴 **margin collapsing 完全未实现**（`solver/block.rs:258-259,411` 各自解析后直接相加，无 `max(+,−)` 步骤）。BFC 边界、`flow-root`、`content-visibility`、RTL。🟡 元素级滚动容器（可滚动溢出 + 滚动条）未做（只有文档级视口滚动 `crates/render-layout/src/fragment.rs:128-175`） | WPT `css/CSS2/`、`css/css-display/`、`css/css-sizing/` |
| Flexbox | Partial | 单行 flex（`crates/render-layout/src/solver/flex.rs`）、`flex-direction` 四值（`:44-49,380-387`）、`flex-basis` 三态（`:751-775`）、`flex-wrap` 与多行（`:184-190,318-323`）、`align-content`（`:184,318`）、auto margin（`:981-986`）、intrinsic measurement 中百分比按 auto 处理（知乎居中修复，`HANDOFF.md` 2026-09-27） | `automatic minimum size`、cross-axis auto margin、`align-self` 覆盖、definite size 传播。🟡 `flex.rs:960-965` 注释自述"the min-content floor is not modeled yet" | WPT `css/css-flexbox/` |
| Backgrounds/Borders | Partial | `background-color`、border shorthand 展开（`crates/render-css/src/cascade.rs:399-404`）、`background-clip` 的 `border-box`/`padding-box` 差异（`crates/render-core/src/paint/display_list.rs` 的 `background_clip_shape:1768` 及其调用点，回归测试同文件）、`linear-gradient`（解析 `properties.rs:1712-1720` → 构造 `display_list.rs:1998` → 光栅化 `crates/render-core/src/paint/raster.rs`）、圆角裁剪（`display_list.rs:1768,1817`） | 🔴 **`radial-gradient()` 未实现**：`DisplayCommand::RadialGradient` 变体（`display_list.rs:286,326`）、光栅化分支、诊断标签（`crates/render-browser/src/diagnostics.rs:44`）都存在，**但解析器不认且无任何构造点**。多层背景、`background-origin`/`position`/`size`/`attachment` 完整矩阵 | WPT `css/backgrounds/` |
| Painting/Stacking | Partial | 基础 display list + CPU 光栅化（`crates/render-core/src/paint/{display_list,raster,scene}.rs`）、六相绘制序 `PaintPhase`（`display_list.rs:336-344`）、`box-shadow`（`display_list.rs:1907` 起 `parse_box_shadow`）、`opacity`、**`text-shadow`（`:1194-1207`）**、**`text-decoration-*`（`:1164-1232`）**、**list marker（`:1298-1420`）**、`transform` 端到端（`transform`/`transform-origin` 发射 + 光栅化纯平移快路径 + 通用仿射 warp）、`contain`、`overflow:hidden` 绘制裁剪（`overflow_clip_shape`） | 🔴 **堆叠上下文只由 `transform` / `opacity<1` 创建**（`display_list.rs:2179` 取 opacity、`:2189` `fragment_stacking_context`）。**`z-index` 只在 block 容器子级做稳定排序**（`crates/render-layout/src/solver/block.rs:763`），排序键是"自身与所有后代 z-index 最大值"这一启发式（`solver/mod.rs:406-425`），绘制层无 z-index 概念，flex/grid 子项不参与排序，`z-index` 自身不创建堆叠上下文。🔴 仍有 10 个属性零消费者（见 Fonts/Text 行）。🔴 **动画/过渡全无**：`@keyframes` 丢弃（`stylesheet.rs:552-557`），`render-core` 内检索 `keyframes`/`animation`/`transition` 无命中。⚠️ **两种显示命令有类型与光栅化但无生产者**：`RadialGradient`（`display_list.rs:286,326`）与 `Canvas`（`:327`）—— `radial-gradient()` 解析器不认（`crates/render-css/src/properties.rs:1712-1720` 只处理 `linear-gradient`），`<canvas>` 无 2D 上下文。⚠️ `text-decoration` 的传播是绘制侧近似而非 cascade 内传播（`display_list.rs:1232-1238` 自述） | WPT `css/css-position/`、`css/css-transforms/`、CSS2 z-order |
| Replaced elements | Partial | `<img>` 加载 + 固有尺寸参与布局、`picture` 源选择（`crates/render-core/src/image.rs:1053`）、**`srcset` + `sizes` + `w`/`x` 描述符**（`image.rs:987-1112,1148`）、格式嗅探（`:412-420`）、PNG/JPEG/GIF/WebP、**自研 SVG 栅格化器**（`crates/render-core/src/image/svg.rs:1-18`，入口 `image.rs:585-586`）、`object-fit` 五个关键字（`display_list.rs:1497,1621`）、损坏/不支持图片的显式错误（`image.rs:521-563`） | 🔴 `object-position` 全仓库无命中。🔴 AVIF 无编解码器（`ImageFormat` 枚举 `image.rs:365-371` 不含 AVIF）。🟡 `srcset` 密度选择用 `device_pixel_ratio_milli`，而该值恒 1000（`image.rs:109`、`crates/render-browser/src/app.rs:1464`）⇒ 高分屏资源选择错误。🔴 `<canvas>` 2D 上下文。`<video>` 只有 poster（`crates/render-js/src/video/mod.rs:40-41,58` 的 `PlaceholderDecoder` 恒 `DecoderUnavailable`） | WPT `html/rendering/replaced-elements/`、`css/css-images/` |
| Inline SVG | Partial | **foreign content 解析已实现**（2026-09-27 新增）：in-foreign-content 判定（`crates/render-html/src/tree_builder.rs:130-183`）、`math`/`svg` 建命名空间元素（`:556-567`）、MathML 文本积分点 + HTML 积分点（`:1237-1278`）、foreign content 规则与 breakout（`:1292-1400,1851-1860`）、XLink/XML/XMLNS 命名空间与 SVG/MathML 标签名/属性名调整表（`:1600-1619,1657,1787-1830`）；`render-dom` 命名空间就绪（`crates/render-dom/src/lib.rs:192,209,500,726`） | 🔴 **几何渲染为零**：`render-layout` 与 `render-core/src/paint` 检索 `Namespace::Svg` 无命中 ⇒ 内联 `<svg>` 及其 `path`/`circle`/`g` 不产生任何几何。做法：把 svg 子树序列化回 SVG 文本喂给**已有的** `image/svg.rs`，注册为 image resource 走替换元素尺寸 | WPT `svg/`（需先有渲染） |
| UA stylesheet | Partial | **已按 WHATWG HTML Rendering 章节重写**（`crates/render-core/src/document.rs:70-211`，142 行，逐节标注 §15.3.1 隐藏元素 / §15.3.2 页面 / §15.3.3 流内容 / §15.3.4 短语内容 / §15.3.6 章节与标题 / §15.3.7 列表 / §15.3.8 表格 / §15.3.10 表单控件 / §15.3.11 `hr` / §15.3.12 `fieldset`/`legend` / §15.5.5 `details`+`summary`）。含 `a:link`/`a:visited` 颜色与下划线、`h1`-`h6` 各自的 `font-size`+`font-weight`+边距、`b,strong{font-weight:bolder}`、`cite,dfn,em,i,var,q,address{font-style:italic}`、`code,kbd,samp,tt,listing,plaintext,pre,xmp{font-family:monospace}`、`big/small/sub/sup`、`ins,u` 下划线与 `del,s,strike` 删除线、`ol/ul` 四级 `list-style-type`（disc/circle/square/decimal）、`mark` 黄底、`dialog:not([open])` 与 `dialog` 定位、`blockquote`/`figure` 缩进、`table{border-spacing:2px}` + `td,th{padding:1px}` + `th{font-weight:bold}` + `caption{text-align:center}`、`hr`、`fieldset`/`legend`；HTML 展现属性（`bgcolor`/`width`/`height`/`align`/`valign`/`cellpadding`/`cellspacing`/`border`/`hspace`/`vspace`）映射为 UA-origin 声明（`document.rs:213-` 起） | 🔴 **表里写入的字体轴声明全部无消费者**（见 Fonts/Text 行）—— 标题与正文字号不同了，但**字重与字体族仍然一样**。🔴 quirks 模式未实现（`document.rs:847-855` 只发一条诊断字符串），而 UA 样式表自述"this sheet is the no-quirks rendering"（`:65-67`）。⚠️ 表内逻辑属性全部物理化书写（左右写死），RTL 需要先给 layout 加 `direction`（表注释 `:50-55` 已声明） | WPT `html/rendering/` |
| URL / Navigation / History | Partial | WHATWG URL 解析（`crates/render-js/src/runtime/builtins/url.rs`）、导航请求与相对解析（`crates/render-core/src/navigation.rs`）、`SessionHistory`、`ReferrerPolicy` **完整九值决策表**（`navigation.rs:350-410`，默认 `strict-origin-when-cross-origin` `:1057`）、`location` 与 `innerWidth`/`innerHeight`（`crates/render-js/src/value.rs:2654-2669`、`runtime/eval.rs:2706-2707`） | 🟡 pushState/replaceState 更新 URL 与 `history.state`，不加载；同文档条目的 back/forward/go 不加载并触发 `popstate`；其它条目重新加载；`length` 按每轮开始时的列表长度报告；跨文档遍历不恢复 `state`。URL Standard 边界（相对 URL 的 path 归一化细节）❓ 未逐一核实 | WPT `html/browsers/browsing-the-web/`、`url/` |

P0 的实施顺序固定为：值和继承 → 文本与 intrinsic size → block/inline → flex → background/paint。
后续算法依赖前一步的 computed/used value，不能倒序用页面参数补偿。
**2026-09-27 追加的顺序约束**：`docs/visual_fidelity_gaps.md` 给出 S2 → S1 → S5 → S3 → S4 → S6 → S7
（UA 样式表 → 字体轴 → 内联 SVG → 零消费者属性 → `@font-face`/`@keyframes` → sticky/z-index → quirks）。
S2 排第一是因为纯 CSS、影响面最大、零架构风险；S1 收益最大但跨 `render-layout` 与 `render-browser`，
必须等持有这两个 crate 的工作收尾。这两条顺序与上面的 P0 顺序不冲突：前者是**能力簇之间**的顺序，
后者是**每个簇内部**的算法顺序。

⚠️ **该顺序的 2026-09-27 状态**：S2（UA 样式表）与 S3 的一半（`text-decoration*`、
`text-shadow`、`list-style*`）已落地；S5（内联 SVG 解析）已落地，只剩渲染侧。
**S1（字体轴）未动，且是当前第一优先** —— 见 P0 的 Fonts/Text 与 UA stylesheet 两行。

---

## P1：可交互页面地基

| 能力簇 | 状态 | 主要缺口 | 验收测试 |
| --- | --- | --- | --- |
| HTML parsing | Partial | 完整 tokenizer 状态、misnested formatting elements（adoption agency）、`noscript` 的 scripting 分支、fragment parsing 边界、解析诊断是否已打印（`docs/qq-compatibility-analysis.md` 记为 ❓） | WPT `html/syntax/` |
| DOM Core | Partial | `Element.append/prepend/before/after/replaceWith/insertAdjacentHTML`、`Element.closest`、Range/Selection 的 JS 侧 API、`document.styleSheets`/`insertRule`/`cssRules`、adoption | WPT `dom/` |
| Events | Partial | **`addEventListener` 第三个参数被完全忽略**（`crates/render-js/src/runtime/builtins/events.rs:106-127`）⇒ `capture`/`once`/`passive` 全部无效；`stopPropagation`/`stopImmediatePropagation` 行为未核实；键盘与指针事件到 DOM 事件的映射 ❓ 部分未核实 | WPT `dom/events/`、`uievents/` |
| Forms | Partial | `input` 的 `text/email/url/tel/number/search/password/checkbox/radio/button/submit/reset` 激活语义 ✅（`crates/render-core/src/interaction.rs:1035-1060`）；**GET 提交端到端 ✅**（`interaction.rs:783-827` 提交计划 + `crates/render-browser/src/app.rs:2048-2069` 可取消 `submit` + `:3009-3017` 导航）；`form` 属性关联 ✅（`interaction.rs:828-856`）；UA 样式表有控件尺寸/边框 ✅（`document.rs:51-54`）。缺口：**POST 提交未接**（`app.rs:3009` 过滤掉非 GET，传输层 `crates/render-net/src/transport.rs:946` 已支持）；`readonly`/`required`/`checkValidity`/`setCustomValidity` 🔴；`file`/`range`/`color`/`date` 类 🔴；`autocomplete`/`pattern` 🔴 | WPT `html/semantics/forms/` |
| Event loop | Partial | task/microtask、Promise jobs、rendering opportunity 三段式**已实现**（`crates/render-core/src/event_loop.rs:23,344,361,443-476,480-508`），含 `max_pending_microtasks`/`max_microtasks_per_checkpoint` 双重上限与显式 `ResourceLimitReached`。缺口：timer nesting 级别、task source 完整集合、`queueMicrotask` 的宿主接线 ❓ 部分未核实 | WPT `html/webappapis/scripting/event-loops/` |
| Navigation/History | Partial | 见 P0 的 URL / Navigation / History 行 | WPT `html/browsers/browsing-the-web/`、`url/` |
| Fetch/XHR | **Partial** | ⚠️ **上一版记为 `Missing`，与实现不符。** `fetch()`、`Response`、经典 `XMLHttpRequest` 已实现（`crates/render-js/src/runtime/builtins/fetch.rs:16,597-803`）。同时 `crates/render-core/src/spec/registry.rs:266-273` 仍把 `fetch.runtime` 登记为 `SupportStatus::Missing` 且 `tests: &[]` —— **登记表滞后于实现，须更正**。真实缺口：CORS 🔴（`crates/render-net/src/lib.rs:3-6` 明确不负责）、credentials 🔴、abort 🔴（传输层有 `CancelToken` 但 JS 侧无 `AbortController`）、同步 XHR 明确抛错（`fetch.rs:729`）、`redirect`/`mode`/`cache` 语义 ❓ 未核实 | WPT `fetch/`、`xhr/` |
| Canvas/Media | Missing/Partial | `<canvas>` 2D 完全不做（`DisplayCommand::Canvas` 无生产者）。`<video>`：元素状态机、呈现帧发布、demuxer/AVC 骨架已有（`crates/render-js/src/video/{present,demuxer,avc,mod}.rs`），**像素解码未写**（`mod.rs:40-41,58`）。`<audio>` 播放与音频输出 🔴 | WPT `html/canvas/`、`html/semantics/embedded-content/media-elements/` |

---

## P1：ECMAScript

test262 基线 **30,808 / 98,096（31.41%）**。按失败簇推进，而不是按站点脚本逐文件打补丁：

1. lexer/parser 与 early errors；
2. execution context、scope、closure、`this`；
3. property descriptors、prototype、Proxy/Reflect；
4. Array/String/Object/Number/RegExp 等基础 built-ins；
5. iterator/generator、Promise、async jobs；
6. module graph、import/export；
7. typed arrays、ArrayBuffer、DataView；
8. Intl、Temporal 等独立大簇。

**每一簇的完成条件**：固定 test262 路径全量运行、适用测试 100% pass、无 crash/timeout、
unsupported 数量有明确下降。总目标可以是固定 revision 的适用测试 100%，但在模块、async 和
host 能力仍被分类为 unsupported 时，不得宣称 test262 100%。

**当前各簇的真实状态**（2026-09-27，证据见 `HANDOFF.md` 2026-09-20 一节与各 crate 源码）：

| 簇 | 状态 | 证据 |
| --- | --- | --- |
| 1 lexer/parser | 大部分 ✅ | 完整 class 语法、`#private`、`??`/`??=`/`&&=`/`||=`、Unicode XID 标识符 |
| 2 scope/closure/`this` | ✅ | `this` 由动态栈改为**环境绑定**（箭头词法 this、派生构造器 `super()` 前 TDZ ReferenceError、逃逸箭头正确）；`new.target` 栈；`crates/render-js/src/runtime/class.rs` |
| 3 descriptors/prototype/Proxy/Reflect | 大部分 ✅ | `runtime/builtins/proxy.rs` 全陷阱接入；`Object.setPrototypeOf` 真语义。❌ 静态继承的边角 ❓ 未核实 |
| 4 基础 built-ins | 大部分 ✅ | `Function`（含 `name`/`length`/`prototype.constructor` 回填）、`String` 包装对象、`Date` 完整语义、`Object` 冻结/密封/扩展性 |
| 5 iterator/**generator**/Promise/async | 🟡 **分裂** | `Iterator` 全局 + `%IteratorHelperPrototype%` + 惰性助手 ✅（`built-ins/Iterator` pass 12 → 356）；`Promise` 有真实原型 then/catch/finally ✅。🔴 **generator/async 方法体仍按普通函数执行** —— `crates/render-js/src/parser.rs:581-583` 源码注释直陈"The runtime does not suspend generator frames"，`methods-gen-*` 一族约 1k 用例失败。🔴 core-js 品牌检查在 Promise 微任务续体上失败（Promise 互操作） |
| 6 module graph | 🔴 | `crates/render-js/src/parser.rs:583` 自述 module 声明被"lowered into the shared page"，`runtime/builtins/global_fns.rs:349` 自述"Module graph fetching belongs to the browser coordinator" |
| 7 typed arrays | ✅ | `runtime/builtins/typed_array.rs` |
| 8 Intl / Temporal | 🔴 | 大量 Syntax/Reference 失败，各自独立大簇 |

🔴 另有两处明确缺口：BigInt 字面量 38 例失败；`[object Object]` 断言簇 638（多与 generator 迭代器相关）。
🟡 超大稀疏 array-like 已由 `MAX_MATERIALIZED_ELEMENTS` 上界 + 显式 `ResourceLimit` 处理
（128 GiB 分配 abort 的根因已修，`HANDOFF.md` 2026-09-20）。

---

## P2：现代应用兼容

- CSS Grid 完整轨道尺寸、隐式网格、**subgrid**（显式轨道 + `auto-fit`/`auto-fill` + `minmax` +
  `repeat` 已有：`crates/render-layout/src/grid.rs`、`solver/grid.rs`；命名线/`grid-area` 密度 ❓ 未核实）；
- **Shadow DOM、Custom Elements、slotting**（全缺。`customElements` 反证：
  `crates/render-core/examples/baidu_diag.rs:245-248` 把它列进存在性探测名单而 `render-js` 无实现；
  `ResizeObserver` 反证：`crates/render-js/examples/qq_bundle_probe.rs:191-192` 自己 polyfill 了它）；
- CSSOM（🟡 内联 `element.style` 已实现，`runtime/builtins/style.rs`；
  `document.styleSheets`/`insertRule`/`cssRules` 🔴）、`ResizeObserver` 🔴、
  `IntersectionObserver` 🟡（`runtime/builtins/observers.rs:60-171`，语义保真度 ❓）、
  `MutationObserver` ✅（`observers.rs:183-350`）；
- **storage 🔴**（`localStorage`/`sessionStorage` 均无）、**cookies 🟡**
  （`document.cookie` getter `crates/render-js/src/runtime/eval.rs:2848-2849` / setter `:3339-3342` /
  jar `runtime/mod.rs:111-112,325-345` / 测试 `runtime/tests.rs:2488`；
  ⚠️ jar 只是 `name -> value` 的 map，**无 path/domain/Secure/SameSite 作用域**；
  传输层另有独立 jar `crates/render-net/src/cookie.rs`，两者是否同源 ❓ 未核实）、
  URL Web API 🟡、Streams 🔴、Encoding Web API 🔴；
- accessibility tree ⛔、IME 🟡（合成闩锁在 `crates/render-browser/src/app.rs:2923,3093`）、
  clipboard ⛔、drag and drop ⛔；
- Cache API ⛔、service worker ⛔、security policy 🔴（CSP 无任何实现）、多进程隔离 ⛔；
  浏览器 shell 的 HTTP 缓存基础设施已单列于当前基线表。

---

## 阶段验收

1. **M1 静态文档**：P0 CSS/布局子集有 WPT 基线，background、字体、block/inline、flex 的
   目标子集无已知失败。
   ⚠️ **2026-09-27 现状：未达成，且阻碍不是算法而是视觉基线** —— UA 样式表已按
   HTML §15 重写（S2 已解，见 P0 的 UA stylesheet 行），但**字体轴结构性缺失（S1）现在
   更刺眼**：UA 表已写入 `b,strong{font-weight:bolder}`、`em,i,cite,dfn,var{font-style:italic}`、
   `code,kbd,samp,tt{font-family:monospace}`、`h1..h6{font-weight:bold}`，
   而这些声明全部无消费者 ⇒ 标题字号变了但**字重与字体族与正文完全相同**。
   另有 quirks 模式未实现（S7）与伪元素无盒子。在 S1 修好之前，
   "静态文档可读"不成立，无论 WPT 数字如何。
2. **M2 交互文档**：DOM、events、forms、navigation、timer/Promise 目标子集通过。
   ⚠️ **现状：引擎侧能力大部分具备，但端到端未达成** —— `docs/qq-compatibility-analysis.md`
   记录的 1364 次 `console.error`、主 bundle `getProto: not an object`
   （其中 `document.cookie` 一项已修复），以及 `HANDOFF.md` 京东一节记录的
   "样式表已取回、离线同 CSS+DOM 全对、在线渲染为裸文本"（已证伪内联 JS 改 DOM，
   剩余变量是外链脚本回放与浏览器侧 commit 竞态）都说明单项能力存在 ≠ 页面可用。
3. **M3 应用启动**：fetch/XHR、module、现代 JS 核心簇可运行常见 hydration/bootstrap。
   ⚠️ **现状：未达成。** `fetch`/XHR 已实现但 CORS/credentials/abort 缺；module 依赖图未做；
   `history` 五个方法是空实现。
4. **M4 媒体应用**：Canvas/Media、资源调度和播放状态机达到视频站基础播放要求。
   ⚠️ **现状：未达成。** `<video>` 像素解码未写（`PlaceholderDecoder`）。

百度、hao123、知乎、腾讯新闻、bilibili、qq.com、taobao.com、京东 仅在每个里程碑末运行一次回归，
失败先归入上表能力簇，再补最小规范测试。实机诊断产物在 `.diag/`，诊断工具是
`crates/render-*/examples/*_diag.rs` 与 `RENDER_DEBUG_FRAME=1` / `RENDER_DUMP_FRAME=<path>`
（转 PNG 用 `tools/ppm2png.py`）。**任何站点失败都必须归约为通用能力缺口**；
`tools/check_site_neutrality.py` 在 CI 中机械拦截站点特判。
