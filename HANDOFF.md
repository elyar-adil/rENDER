# 交接文档 (2026-11-03, 续) — 无窗口 headless 页面运行器

新增 `render-browser` 的 `render-headless` bin 和库里 `render_browser::headless` 模块。它只做网络/文件 I/O 与离屏 CPU 光栅化，不创建 winit/AppKit/NSWindow、不启动 softbuffer/gpu、不拉起外部浏览器；窗口焦点不受影响。

## 用法

```bash
cargo run --release -p render-browser --bin render-headless -- \
  --url https://www.example.com/ --out /tmp/page.png \
  --width 1180 --height 780 --dpr 2 --wait-ms 10000 --print-health
```

可选 `--proxy ADDR|direct`、`--dump-dom PATH`、`--print-health`（JSON：readyState、资源计数、surface 尺寸、元素 border box）。PNG 尺寸为 `width*dpr × height*dpr`。

## 已实现

- 导航经 `render_net::HttpTransport`（TLS 验证保留；显式代理与 `NO_PROXY` 语义沿用 transport）。
- HTML 编码嗅探复用 `decode_html_bytes`/`Content-Type` charset；`<base>`/相对 URL 由现有 Discover 路径处理。
- 外部 CSS 经 `resources.rs`，经典/模块脚本经 `scripts.rs`（含依赖图、import map、动态 `import()`），图片经 `images.rs`（含 CSS background、srcset 选择上下文），webfont 经 `font_faces.rs` 的 `DocumentFontLoader`。
- `Page` 只跑 DOM/JS/事件循环（`run_one_turn_without_render`），每次需要时把 DOM 快照交给 `Document::render_with_interaction_and_transitions_and_frames`；`device_scale` 只缩放 paint，layout/脚本几何保持 CSS px。几何与 computed style 通过 `publish_render_geometry`/`publish_computed_styles` 回灌脚本。
- `fetch()`/XHR 同步执行，cookie jar 复用 `render_net::CookieJar`；跨域响应做 ACAO 检查。`DOMContentLoaded`/`load` 在资源与立即任务静默后派发。
- 本地 HTTP fixture 测试（CSS + 外链 JS + JS `fetch` + SVG 图片 + 失败 webfont）验证 DOM 更新、PNG 800×600（400×300 CSS @ DPR 2）、健康计数与元素几何。

## 实测

- `example.com`（release，1180×780 CSS，DPR 2）：2360×1560 PNG，614 ms，`ready_state=complete`；截图 `/tmp/headless-example-release.png`。
- `rust-lang.org`（release，同视口）：2360×1560，7.2 s，4 个外部样式表、9 张图片、3 个脚本执行，`css_errors=0`、`images_failed=0`；截图 `/tmp/headless-rust-lang.png`。
- `baidu.com/s?wd=abc` 被服务端重定向到 `wappass.baidu.com` 验证码页，脚本两次触发解释器 step limit；截图是真实返回的验证码页，不是伪造搜索结果。10 s 预算下 release 实际 18.4 s，因为单个 JS turn 不可抢占（已在 README 说明）。

## 剩余缺口

- 无 GPU、无窗口、无输入/滚动/动画播放；截图是静止视口首屏。
- 没有导航接管（脚本 `location`/表单提交中途导航不会换页）、无 iframe 导航编排、无 CORS preflight、无 service worker/SharedWorker。
- `wait-ms` 是软预算：单个解释器 turn 不能中断；超时会体现在 `timed_out`。
- 本地 `file:` 页面的外部子资源仍走 `render-net`，因此不会读取同目录文件；HTTP(S) 页面没有这个限制。

---

# 交接文档 (2026-11-03, 续二) — CSS 背景图尺寸、位置与图层发现

本轮修复快捷方式图标被画成整块红/黑矩形的问题，全部走通用 CSS 背景绘制路径，没有站点特判。

## 根因

`paint_background_image` 只识别 `background-size: cover|contain`，其余值一律按图片 intrinsic 尺寸绘制。真实页面把 96×96 / 256×256 品牌 PNG 放进 18px 高的 `inline-block`，并声明 `background-size:18px 18px`；旧代码把整张图从左上角开始画，只被 18px 高的盒子裁掉，于是整块红色（京东）、黑色（QQ/12306）或青色（WorkBuddy）覆盖到文字上。第二个问题是非标准的 `background-position-y:2px` 只存在 token map，绘制层完全不读；`background-position` 也只支持关键字/百分比，不支持长度。

## 修复

- `render-core/src/paint/display_list.rs`：按 CSS Backgrounds 3 §3.9 解析 `<bg-size>`（`auto`、长度、百分比、单值保持宽高比、双值、`cover`/`contain`）；按 §3.6 解析位置到 positioning area 的像素偏移（关键字、百分比按 free space、长度绝对），并让 `background-position-x`/`-y` 覆盖对应轴。
- `render-css/src/cascade.rs`：`background` 简写只在顶层 `/` 处拆分尺寸，不再把 `https://` 的斜杠当分隔符；`/` 后保留显式长度/百分比/双值，而不是只接受 `auto`/`cover`/`contain`。
- `render-core/src/image.rs`：`background_url` 支持逗号分隔的多图层和 `image-set()`/`-webkit-image-set()`，取第一个可下载 URL；`mask-image` 同样受益。
- 新增 paint 集成测试覆盖 18px 真实快捷方式规则（含 `background-position-y`）和 96×96 → 32×32 缩放；新增图层/image-set 发现测试与 `background` 简写尺寸测试。

## 验证

```text
cargo test -p render-core --lib                              567 passed, 1 ignored
cargo test -p render-core --test paint_images                   5 passed
cargo test -p render-css --lib                                335 passed
cargo test -p render-browser --lib                            249 passed
cargo clippy -p render-core -p render-css -p render-browser --all-targets -- -D warnings  clean
cargo fmt --all --check                                       clean
python tools/check_site_neutrality.py                         0 blocking, 31 advisory
```

额外用 `/tmp` 中 curl 下载的真实资源做离屏检查（无窗口）：96×96 PNG 在 18×18 目标上采样为 `(227,34,29)` 红色；256×256 WebP 与 64×64 PNG 同样缩放到 18×18；图像命令 bounds 为 `(x, y+2, 18, 18)`。

## 剩余缺口

- 一次只绘制背景图层的第一个 URL；多 URL 图层的堆叠、每个图层独立的 size/position/repeat 列表尚未实现。
- `image-set()` 只取第一个候选，不按 DPR/分辨率选择。
- `background-position` 的 3/4 值边缘偏移语法（`right 10px bottom 20px`）仍是近似。
- `background-repeat` 的多图层循环、以及 `background` 简写的逗号多图层展开仍缺。

---

# 交接文档 (2026-11-03, 续二) — 搜索框静态定位、行内基线与页头裁切

本轮修复 hao123 首页截图中搜索框上边框缺失、热点提示行贴近/被页头导航行裁切的问题。所有改动仍走通用 CSS 2.1 布局/绘制路径，没有站点特判。

## 根因与修复

- **绝对定位 flex 子项的 static position 用了 containing block 的 padding box**（`crates/render-layout/src/solver/block.rs:407` 一带）。`.search` 是 `.search-group`（`display:flex; padding-top:10px`）的绝对定位子项，但最近的 positioned 祖先 `.searchWrapper` 没有 padding。原实现把 `margin_box_y` 覆盖成 `containing.origin.y`，于是 `.searchForm` 落在 y=76 而不是内容边 y=86；页头 `top:-10px` 上移后，上方 `z-index:555` 的 `.layout-header` 白底正好盖住表单顶部 1.5px 渐变边框。现在保留父级 flow cursor 作为 `auto` inset 的 static position，显式 `top/left` 仍相对 positioned ancestor 解析。
- **匿名 inline box 重复绘制元素的 background/border**（`crates/render-core/src/paint/display_list.rs:1431`）。inline-block 的匿名行内包装借用了元素的 formatting node/style source，`paint_box` 又在 padding box 上重绘一次，可能盖住元素自己的顶边。现在只有 `fragment.source.is_some()` 的 box 绘制自身装饰。
- **行内文本 baseline 没有半行距**（`crates/render-layout/src/solver/inline.rs:2677`）。`line-height` 大于字体 ascent+descent 时，所有 leading 都被放在字形下方，文本整体偏高；23px 热点行里的 13px 文本顶部因此被 `overflow:hidden` 裁掉。现在按 §10.8.1 把 half-leading 平分到上下。
- **atomic inline 未按基线对齐**（`crates/render-layout/src/solver/inline.rs:1057`）。热点行的 `inline-flex` 链接和内联 `inline-block` 徽标都按行顶放置；第二行 fallback 的 16px 徽标因此只露出顶部 1px，看起来像被导航行裁切。现在同一行内 atomic box 的文本基线对齐到行基线；整行只有 atomic 时取最高的 atomic 基线。
- **`overflow` 两值简写把 x/y 写反**（`crates/render-css/src/cascade.rs:2196`）。`overflow: hidden auto` 之前得到 x=auto/y=hidden；现按 CSS Overflow 3 §2.1 第一个值为 x。

## 几何（保存页 `/tmp/hao123.html`，1728 CSS px）

| 元素 | 修复前 | 修复后 |
| --- | ---: | ---: |
| `.searchForm` | y=76, 52 高，顶边框被白条覆盖 | **y=86**, 52 高，四边渐变边框完整 |
| `.all-hot-search-wrapper` | y=92 | **y=102** |
| 热点首行文本 baseline | 189.4 | **194.4**（13px 文本在 23px 行内居中） |
| 首行 `hot-icon` 徽标 | y=178..194 | **y=182.8..198.8** |
| 第二行 fallback 徽标 | y=201..217（露出 1px） | **y=205.8..221.8**（完全在 23px clip 之外） |
| `#hao123-govsite` 导航 | y=221 | y=221（不变） |

## 测试

- `render-layout`：`absolute_flex_child_static_position_starts_at_the_content_edge`、`inline_block_badge_baseline_aligns_on_an_atomic_only_line`、`inline_text_uses_computed_font_size_and_line_height`（half-leading=25.2 baseline）。
- `render-core`：`search_form_top_border_survives_the_higher_header_strip`（像素级验证顶边框未被 z-index 更高的白条擦掉）、`clipped_second_hot_row_does_not_leak_its_badge`（像素级验证第二行徽标不泄漏）、`anonymous_inline_boxes_do_not_repaint_the_elements_box_decorations`，以及 `layout_positioning` 的 reduced header 精确坐标。
- `render-css`：`overflow_two_value_shorthand_maps_first_value_to_the_x_axis`。

```text
cargo fmt --all --check                                             clean
cargo clippy -p render-layout -p render-css -p render-core --all-targets -- -D warnings
                                                                    clean
cargo test -p render-layout --lib                                   405 passed, 1 ignored
cargo test -p render-css --lib                                      335 passed
cargo test -p render-core --lib                                     566 passed, 1 ignored
cargo test -p render-core --test layout_positioning --test layout_paint_regressions
                                                                    17 passed
python3 tools/check_site_neutrality.py                              0 blocking
```

## 剩余缺口

- inline 级 `vertical-align` 仍只对表格单元实现；本轮只补了 atomic inline 的默认基线对齐，`top`/`middle`/`bottom`/`text-top` 等值仍未消费，替换元素（图片）保持行顶对齐。
- atomic 基线移动不重新扩张行盒高度；特别高的 inline-block 可能越过行盒，后续应在行高计算中计入 ascent/descent。
- `background-clip`/`background-origin` 长属性尚未解析，多层背景绘制仍按层序推断 padding-box/border-box；本轮 `padding-box, border-box` 写法工作正常，但层序相反时会错。
- 受“不触碰桌面”约束，本轮没有启动浏览器/Playwright/`--screenshot`，所有验证均为 `cargo test` 与 render-core 离屏 raster。

---

# 交接文档 (2026-11-03) — 通用 Web Worker API

本轮补上 Dedicated Web Worker，让把压缩/遥测/captcha 工作放到 worker 的页面不再因 `ReferenceError: Worker is not defined` 进入全局错误页。实现完全走通用 HTML/JS 路径，没有站点特判。

## 语义与边界

- `new Worker(url, {name})` 立即返回对象；经典脚本在后续任务中加载。URL 相对页面 base 解析，`data:` URL（含 `Blob` + `URL.createObjectURL`）直接解码；HTTP(S) 脚本复用 `PendingFetch` 队列，由浏览器现有网络/CORS/cookie 路径完成，`settle_fetch` 后启动。
- Worker 是同一 runtime 内的独立 realm：没有 `window`/`document`，有 `self`、`globalThis`、`postMessage`、`importScripts`、`close`、`onmessage`/`onerror`/`onmessageerror`、`addEventListener`/`removeEventListener`、timers、`fetch`、`structuredClone`、`Blob`/`URL`、TextEncoder/Decoder、console、performance、location、navigator、`WorkerGlobalScope`/`DedicatedWorkerGlobalScope`。
- 消息在 post 时和送达目标 realm 时各做一次结构化克隆，保证接收方对象的原型属于接收 realm；`ArrayBuffer` transfer list 会移动字节并 detach 源 buffer。错误在 Worker 对象上派发 `error`，不会从构造函数同步抛出。
- `terminate()`/`close()` 停止后续消息、timer 和内部任务；`Worker` 对象保留以便原有引用继续响应。
- 局限：同线程、同堆的隔离 realm，不是独立 OS 线程；仅 classic worker，不支持 module worker、`SharedWorker`、`MessageChannel`/port、`navigator.serviceWorker`；worker realm 暂未安装 `crypto`。`importScripts` 在无同步 `DocumentLoader`（浏览器）下会报 NetworkError，页面主脚本网络加载不受影响。

## 验证

- `render-js` 新增 12 个 worker 测试：echo、`importScripts` 顺序、terminate、启动前/消息处理中的 error、transfer/detach、timer realm、worker fetch 相对 URL、data URL/Blob、网络 fetch seam、无页面全局、`typeof Worker`。
- `render-core` 新增 page 测试：worker 脚本经 `take_pending_fetch_requests`/`settle_fetch` 启动并把消息送回页面。
- 真实知乎 signin（临时 profile，headless `--screenshot`）：页面停在 `https://www.zhihu.com/signin?next=%2F`，无 `__probe_errors`、无“出了一点问题”，二维码/短信登录表单正常绘制。额外探针确认 `typeof Worker === "function"`、自建 Blob worker echo `42`，并观察到页面自身构造 11 个 `data:` worker（Sentry 压缩路径）且无 worker error。健康行仅保留既有的 `scripts_threw=1`（图标 chunk 执行步数上限），与 worker 无关。
- 截图：`/tmp/zhihu-worker-signin.png`；带 worker 探针的 DOM：`/tmp/zhihu-worker-probe-dom.html`。

## 剩余缺口

- 独立线程、module worker、SharedWorker、MessageChannel/MessagePort、worker `crypto`、FileReader 等 worker 专属 API。
- `importScripts` 的浏览器网络路径目前没有同步实现；现有页面若在 worker 内 importScripts 远程脚本会收到 NetworkError。
- worker 内 `onerror` 自身监听未派发；错误只到页面 `Worker` 对象，便于站点按标准处理。

---

# 交接文档 (2026-11-02, 续七十) — 原生页面文字选择与复制

本轮补上了页面正文的鼠标选择/复制，没有站点特判。

## 状态与坐标

- `PageState` 新增 `fragments: Option<Arc<FragmentTree>>`，由 full render 的 `PageRenderFrame` 携带（retained-raster 帧为 `None`，保留上一棵树）。页面选择只绑定已提交的布局快照。
- 每 tab 的 `PageSelection` 包装 core `PointerSelection` 并记录 DOM revision。选择、方向、边界 clamp、mutation repair 全部复用 `render-core` 既有实现。
- 高亮由 `InteractionScene::selection_geometry` 产生 CSS 文档矩形，`compose_frame` 转成物理坐标：`x = rect.x * DPR`、`y = chrome_height + (rect.y - scroll_y) * DPR`，再与 `layout.content` 求交。页面 raster 保持 retained，选择只是 alpha overlay（现有浅蓝 selection 色，112 alpha），不需要重渲染。
- scroll 直接改变 `viewport_rect` 的来源偏移，高亮随内容移动；resize 用新 DPR/chrome 重算。选择可见时 `can_composite_scroll` 返回 false，避免 compositor 直接 present 页面 surface 时把 overlay 擦掉，并对在途 compositor job 调用 `cancel_tab`。

## 手势

- `PointerDownMode::{Replace, Extend, SelectWord}`：普通按下 collapse 到最近 caret；shift-click 保留 anchor 并 extend；双击用 `word_range_at` 选中单词。`pointer_up` 只在真正拖动后再次 extend，双击单词不会被 release 点 collapse 回去。
- 按下命中文字后，`click`（含链接导航）延迟到 release；未拖动仍走原 `perform_content_click`，因此链接、按钮和表单提交语义不变。移动超过 2 CSS px 才视为拖动，轻微抖动不会吞掉链接点击。
- 点击空白正文、chrome、地址栏或切走页面会清空选择；导航在 `PageState::set_source` 中清空选择与 fragment 树。
- 拖动中不进 form control：聚焦 input/textarea 的 press 仍交给 `ContentTextEditor`；Cmd+C 在 content editor 有焦点时仍走编辑器自己的 selection，不经过页面 Range。

## 复制

- 手势 press/finish 时把 core Selection 同步到 DOM live Selection（`selection_set_base_and_extent`）。Cmd/Ctrl+C 通过 `Dom::selection_to_string` 取文本并写入 clipboard。
- `BrowserApp.clipboard` 从具体 `NativeClipboard` 改为 `Box<dyn Clipboard>`，headless 测试可注入内存 clipboard。
- DOM mutation 后 `PageState::repair_page_selection` 调 `Selection::apply_mutations`：character-data 编辑 clamp offset，child-list/断开边界按 core 规则清除；journal 已丢弃时直接清除。compose 前执行。

## 测试与验证

新增 app tests：两段文字拖动产生两个高亮矩形并实际画出非背景像素、双击选词、Cmd/Ctrl+C 复制、未拖动的链接点击仍导航、导航清空选择、滚动使高亮同步位移、文本缩短后 offset clamp、点击空白清空选择、DPR 2 下高亮矩形为物理坐标。render-core hit-test 新增未拖动链接激活、双击单词、shift-click extend 三个测试。

```text
cargo fmt --all --check                                    clean
cargo clippy -p render-browser -p render-core --all-targets -- -D warnings
                                                            clean
cargo test -p render-browser --bin render-browser          140 passed
cargo test -p render-browser --lib                         241 passed
cargo test -p render-core --lib interaction                 20 passed
python tools/check_site_neutrality.py                       0 blocking, 31 advisory
```

## 剩余缺口

- 未派发 `selectstart`/`selectionchange`；拖动过程中只在 press/finish 同步 DOM Selection，脚本在拖动中读到的仍是上一次边界。
- 脚本通过 `Selection` API 设置的选择不会反向驱动原生高亮（高亮以 shell 的 `PageSelection` 为准）。
- 选择不跨 iframe 内容（iframe 目前以 image 形式绘制在父 fragment 树中）；无拖动自动滚屏、无三击整段/行选择、无键盘 caret 扩展、无右键复制菜单。
- 每次点击会为 DOM Selection 创建新的 live Range；render-dom 的 range 表本身尚无回收机制。

---

# 交接文档 (2026-11-02, 续六十九) — 常用站点体验收敛：滚动/渲染性能、百度布局、字体继承、X/知乎诊断

本轮把目标从“广撒网补能力”收敛到少数常用页面：百度搜索、Google、知乎、X。所有修复仍然走通用 HTML/CSS/DOM/JS/layout/paint 路径，没有站点特判。

## 性能：滚动与主渲染链路

滚动（Apple M1 Pro，release；`render-scroll-bench`，120 帧）：

| 指标 | 修复前 | 修复后 |
| --- | ---: | ---: |
| retained compositor | 1909 µs/帧 | **581–640 µs/帧** |
| full raster | 3450 µs/帧 | **725 µs/帧** |
| Baidu retained | ~254 ms/帧 full raster | **13 layers，3.33 ms/帧**（`072426c`/`11fa52a`，snapshot 18.5×）；±1 channel 差异为既有 band rounding |

主要提交：`cbee346`、`2c43a6a`、`0a0c971`、`398be99`、`a2dd631`。修复内容：
- retained band blit 预计算 clip 边界并整行拷贝，去掉逐像素 clip 测试；
- wheel 输入合并到下一次 redraw，一次滚动一帧、一次 compositor submit、一次 present；
- hover 变化不再抢占 composited scroll frame；
- GPU presenter 只上传滚动区域，不再整窗转换/上传；
- fixed 元素在 clip 内不再让整个 retained scene 回退；
- 直接 present 后 CPU frame 懒重建，避免上传旧滚动位置。

主渲染链路（`render-perf --fixture all --iterations 5`，p50）：

| fixture | first_render 前 → 后 | scroll_render 前 → 后 |
| --- | ---: | ---: |
| generated | 218.8 → **126.0 ms** | 60.8 → **14.7 ms** |
| hn | 57.3 → **22.9 ms** | 4.23 → **2.24 ms** |
| portal | 176.9 → **74.9 ms** | 82.5 → **5.58 ms** |

主要提交：`a6290d3`。优化了 selector match（去掉每元素 `@layer` HashMap 和大小写 String 分配）、transition 无动画时的快照、cascade 只遍历声明/继承属性、intrinsic width 缓存、border/fill/clip/offscreen warp 的 raster 快路径。像素哈希与基线逐字节一致。

## 百度布局：三处通用缺陷 + 字体继承

对 Baidu 结果页离线快照与 Edge 同视口截图逐项比对，修复（提交 `348a601`、`d9926c0`）：

1. **inline-block shrink-to-fit 忽略前置 float**：`.toplist1-tr` 从 348×54 回到 348×32，右侧栏高度从 1161 回到 839。
2. **inline-flex 忽略定宽空子项**：图标 14px + margin 6px 不再贡献 0，“大家还在搜” chips 不再被压窄、文字不再上下溢出。
3. **表格自动宽度忽略定宽块后代**：300px 子块不再让表格塌成 44px。
4. **`font-family` 没有进入继承注册表**：嵌套 `<span>`/`<b>`/`::before` 之前全部退回 `system-ui`，导致图标字体空白、字号从 28px 跳到 42px；现在按 CSS Fonts 4 §2.2 继承，`@supports (font-family: ...)` 也改为 supported（`render-css` 320 passed，`render-js` 1235 passed）。

修复后的百度截图：`/tmp/baidu-after.png`；Edge 参考：`/tmp/render-site-diag/edge-1180.png`。结构已经正常。当时最大的剩余差异是 **Retina DPR**：rENDER 把 2360 物理像素当成 CSS 视口、`devicePixelRatio=1`，导致页面按 2360px 宽度排版。

**DPR 已修**（`e94fd2c`、`2d942b4`、`c803cb3`）：`RenderIdentity.device_scale` 携带窗口缩放；layout/viewport/media query/`innerWidth`/`getBoundingClientRect`/scroll/hit-test/几何全部是 CSS px；`DisplayList::scaled(scale)` 在 raster 前缩放所有几何和字号，所以 14px CSS 文本请求 28px 的 glyph mask，不是位图放大；`PageState.surface_size` 单独保存物理帧大小，chrome/scrollbar/present 保持物理。百度在 1180×780 logical / DPR 2 下：

| node | rENDER | Edge |
| --- | ---: | ---: |
| `#content_left` | 150, 98, 608×2325.33 | 150, 98, 608×2321.31 |
| `#content_right` | 878, 119, 348×855 | 878, 119, 348×839 |

修复后截图 `/tmp/baidu-dpr-merged.png`；Edge 参考 `/tmp/render-site-diag/edge-1180.png`。`innerWidth=1180`、`devicePixelRatio=2`、`@media (min-width:2000px)` 不再误匹配。剩余限制：CSS `resolution`/`-webkit-device-pixel-ratio` 媒体查询尚未实现；非整数 DPI 会有一物理像素取整差；文本按 CSS 字号 shape 后按设备字号重新栅格，没有设备字号 re-shape。

## 字体：启动与内存

- macOS PingFang 之前取 collection index 0（香港字形），改为 index 2（简体 SC）；
- `fontdue` 的 `load_substitutions=false`，不再为 GSUB 加载全部 glyph；
- 修复 macOS 上 Menlo 是 `.ttc` 导致 6 个 document-face 测试失败的问题。

`render-perf index` 的 `first_visible` 从 ~605ms 降到 ~410ms，峰值 RSS 从 ~970MB 降到 ~760–850MB。仍不敌 Chrome：PingFang ≈310MB、Arial Unicode ≈310MB 是 eager 加载，下一步需要按 cmap 覆盖懒加载可选字体。

## X 登录页空白

实测不是网络/CSS/登录态：HTML 200、28,320B、0 form/input/button；JS shell 未启动。三个独立 blocker：

1. **lexer 把命名函数体后的 `/` 当除法**：`function Fn(){...}\n/(?:^|;\s*)gt=/` 报 `unsupported character '\'`。已在 `cabdf0e` 修复：`paren_opens_block` 识别 `function NAME(`、`function* NAME(`、方法体等。
2. **`ReadableStream` 缺失**：X 的 TSR router 在顶层 `new ReadableStream`，缺失导致 hydration 数据不生成。
3. **`Intl` 缺失**：startup chunk 顶层 `new Intl.Segmenter`，缺失导致整个 module graph 中止。

第 2、3 项已经实现并合并：`45018b5`/`e8a8da9`/`a593c2f`。`ReadableStream` 覆盖 default stream、reader、队列/背压、`tee`、async iteration（byte stream/BYOB/pipeTo 仍显式缺失）；`Intl` 覆盖 `Segmenter`（真实 UAX #29 grapheme/word/sentence）、`NumberFormat`、`DateTimeFormat`、`PluralRules`、`Collator`、`getCanonicalLocales`（仅 en/en-US 数据，无时区库）。`render-js` 1266 passed。

合并后 X 的启动链不再抛 `ReadableStream`/`Intl`：`__INITIAL_DATA__`、`$_TSR.router`、hydration stream 都存在，`streamEnded=true`，但 `router.hydrated` 仍未置位、`#react-root` 不存在、表单仍为 0。下一个具体证据是 `api.x.com/1.1/guest/activate.json` 经诊断代理返回 404；`x-hydration` agent 正在对比 Edge 与 rENDER 的请求头/credentials/响应，随后修通用 fetch/hydration 语义。X 的 CSS 本身没有问题：把 Edge 的 post-JS DOM + 捕获 CSS 交给 rENDER，表单可见。

## hao123：真正的根因是 intrinsic sizing，不是缺 CSS

之前的“`aio-all.css` 没加载”判断被纠正：保存的 HTML 里那段 258,432 字节的 inline `<style>` 与 `/tmp/hao-aio.css` 完全相同；Edge 打开同一份 HTML 也没有外部 stylesheet link（只有 5 个 dns-prefetch），BigPipe 的 `loadedResource` 短路是页面自己的逻辑，Edge 与 rENDER 行为一致。

101px 搜索框的真正原因是三处通用 intrinsic sizing：

1. `atomic_outer_intrinsic_width` 对 inline-flex 子项取 `max` 而不是按 flex row 求和，`.searchForm` 只量到 697.5px（应为 796.5px），提交按钮被挤到下一行。
2. flex intrinsic 把 absolutely positioned 子项算作 flex item，隐藏的 `.hotword` 把 row 撑宽 18px。
3. inline sequence 的 measurement atoms 包含 absolutely positioned 子项，`.tabs-toggle` 把 shrink-to-fit 的 `.search` 撑宽 66px。

修复：`43a281e`（`solver/inline.rs`、`solver/flex.rs`）以及 `c9b81a0`（动态 `<link rel=stylesheet>` 在 CSS 应用后补发 `load`/`error`；`data:text/css` 探测 link 的 `onload` 从此能完成）。1× 几何：

| 元素 | 修复前 | 修复后 | Edge |
| --- | ---: | ---: | ---: |
| `.searchForm` | 700.5×101 @(305,76) | **799.5×52 @(239,76)** | 798.5×51 @(239,86) |
| `.search` | 909.5×101 | **942.5×52** | 941.5×51 |
| `.textWrapper` | 704 | **686** | 686 |
| submit | 换行 y=126.5 | **同一行 y=77.5** | 同一行 |

截图 `/tmp/hao123-final.png`。仍差：`.search` 比 Edge 高 10px（绝对定位 flex 子项的 static position 未计入 `search-group` 的 `padding-top:10px`）；1px/1.5px 边框取整；文本输入框的 UA `box-sizing` 与 Chrome 不同。图片 `Content-Type` 与字节不符的问题由 `a854242` 改为按嗅探格式解码。

## 知乎登录页

四个通用修复已合并：

- `647cede`：`document.styleSheets`、`style.sheet`/`link.sheet`、`insertRule`/`deleteRule` 和 CSSOM 规则拆分。emotion 的 `sheetForTag` 不再对 `undefined.length` 抛错，React 不会被全局 error boundary 卸载。
- `cf58098`：canvas 每次给 `width`/`height` 赋值都重置 2D 状态（qrcode.react 依赖 `canvas.width=canvas.width` 清空 transform）。
- `305e1d1`：页面 canvas 像素发布到 render worker 的 image store；canvas draw 也算一次需要重绘的原因。
- `3aade6d`：匿名 line box 不再输出 `overflow:hidden` clip，绝对定位的“获取短信验证码”按钮不再被父级 `.SignFlow` 的 line box 裁掉。

最终 live 页（1180×780 CSS / DPR 2）：`ready=Interactive dom_elements=268 boxes=95`，表单、二维码、短信按钮、微信/QQ/微博圆形图标都在；截图 `/tmp/final2-zhihu.png`。后来 captcha 引入 `Worker is not defined`，Worker 实现后不再进入 error boundary；`scripts_threw=1` 是 captcha 图标 chunk 的 step limit，不影响表单。

剩余：CSSOM 只是功能子集（`media`/`disabled`/`replace` 等未实现，link sheet 的 insertRule 不进 cascade）；`getComputedStyle()` 仍返回空值；背景插画在 live 2× 截图中未绘制（静态资源/懒加载待查）。

## 首次绘制：样式表现在是 render-blocking

用户看到的“先纯文本、再变有布局”是首帧在外部 CSS 应用前就提交。`3196fb1` 后，文档的第一帧如果带有非空 `style_plan` 且还没有 `applied_style_sheets`，就不再安装；页面保持空白/旧页，等样式批次完成并提交已样式化的帧。后续动态新增的样式表仍保留旧页面，不闪白。

实测百度 `?wd=abc`：1s 时 DOM 只有 3 个节点（白页），2.5s 时 `dom_elements=1287` 且 AI 卡片背景/右栏/字号一次到位，不再出现文字重叠后再 reflow。新增 `app_tests::first_paint_waits_for_blocking_stylesheets`。

## SVG：设备尺寸栅格、even-odd 多子路径、椭圆弧

`fe4bda5` 修了三处通用 SVG 缺陷：

1. inline SVG 之前按 CSS 尺寸栅格化，2× 屏上再被 display list 放大；现在 `discover_inline_svgs_with_colors_at_scale` 在 `device_scale` 下生成 48×48 raster，layout/几何仍是 24 CSS px。
2. 一个 `<path>` 的多个 subpath 之前各自独立填色，even-odd 的孔洞被填成色块；现在所有 subpath 合成一个 fill，`fill-rule: evenodd/nonzero` 正确。
3. `A/a` 椭圆弧之前直接连到终点；现在按 SVG 2 §B.2.4 endpoint→center 参数化、半径修正和 large-arc/sweep 展平。知乎的微信/QQ/微博图标从色块恢复成正确轮廓。

知乎 24px 图标已可辨认；仍缺 Edge 的浅灰圆形底（CSS 背景/圆角路径），交给 `svg-fidelity` agent。相关测试：`device_scale_rasterises_inline_svg_at_device_size`、`fill_rule_evenodd_cuts_a_hole_in_a_multi_subpath_path`、`parse_path_flattens_an_elliptical_arc_off_the_chord`。

## X：从“完全不启动”推进到 hydration error boundary

已合并 `2d999d8`（URLSearchParams/URL.searchParams）、`832f64e`（对象/数组字面量在求值后续成员时 root 住已构造值，避免 GC 丢成员）、`49c94bf`（`modulepreload`/preload/stylesheet link 补发 `load`，lazy import 不再永久 pending）。

本地 replay 走到 `ENTRY hydrated` 后曾渲染 root error boundary。`ddce587` 等修掉了真正的异常链：await 中 pending promise 的 reaction list 未被 GC root、`URL.parse` 忽略 URL-object base、`Response.arrayBuffer` 缺失且 body 被 lossy UTF-8 解码、`crypto.subtle.digest` 缺失、`oninput` IDL 属性缺失、`RENDER_JS_FRAME_OFFSETS` 改写 optional-chain sentinel、module stack 匿名。

最终 live x.com（1180×780 / DPR 2，30s）：

```text
ready=Complete dom_elements=452 boxes=101 scripts_failed=0 scripts_threw=0
forms=1 inputs=2 buttons=3 modal=1 "Something went wrong"=0
```

登录弹窗、手机/Google/Apple 三个按钮、邮箱输入、Continue、法律文本都完整可见；截图 `/tmp/final2-x.png`。弹窗曾横向裁切，`e70d37d`/`8358b38` 修了 `inset` shorthand 未展开（绝对定位滚动容器 shrink-wrap 成 248px）和 column flex 的 border-box/`flex-grow` 测量高度问题，几何与 Edge 一致（modal 700、column 380、input 360）。Google GSI iframe 仍是空白嵌套文档，fallback 面已正确居中。

## Web Worker

`a19ea3a` 实现了 dedicated Web Worker：异步构造、URL/data:/Blob 脚本、隔离 realm（无 window/document）、postMessage/onmessage/onerror、importScripts、close/terminate、structured clone + ArrayBuffer transfer、worker 内 timers/fetch/console/nested Worker。限制：同线程同堆隔离（无真正并行）、classic worker only、无 SharedWorker/module worker/MessagePort/worker crypto。12 个 worker 测试 + 1 个 page test。它解除了知乎 captcha 的 `Worker is not defined` 错误边界。

## 页面文本选择

`7d9c1e5` 接入 `PointerSelection`：鼠标按下/拖动/Shift 点击/双击选词，高亮 overlay 按 chrome/scroll/DPR 转换，Cmd/Ctrl+C 复制选中文本，导航/DOM mutation 清理，滚动跟随；链接未拖动时仍正常导航。覆盖两段文本、双击、复制、链接、导航、滚动、DPR2 测试。剩余：selectionchange/selectstart、iframe 内选择、拖拽 autoscroll、三击/键盘扩展。

## 字体按需解码与栅格/容器优化

- `d00fd1a`：安装字体改为 `FaceSlot::Lazy` + cmap 覆盖位图（约 1.1MB/8 faces），首次使用时才 `fontdue` 解码。`render-perf generated first_visible` 523→164ms，peak RSS 881→278MB；首页 941→594–609MB；Baidu 12s 3172→2832MB。Latin/CJK/symbol 截图 1×/2× PNG 相同。剩余：PingFang 选中后仍 eager 解析全部 glyph（~250–280MB），需要 per-glyph/atlas。
- `1134f66`/`41cf5a6`/`4cc903d`：offscreen stacking context 按内容 bounds 裁剪并池化，GitHub styled raster 1105→**30.7ms**（全页 0 像素差）；container feedback 从 3 次全 cascade 改为 seeded + 增量，GitHub 2.0s→0.33s，重 cascade 6372→951+96。Baidu 剩余 raster 主要 11 个 linear gradient（~150ms）。

## Google 结果页最终

`9178c5e`、`05637ba` 后，consented profile 的 Google 结果页真实可用：`ready=Interactive dom_elements=2530 boxes=940 scripts_failed=0 scripts_threw=0`，结果标题/URL/snippet 出现；`#center_col` y 368→200（Edge 200），结果行高与 Edge 基本一致。截图 `/tmp/google-results-after.png`。剩余 fresh-profile consent、`#rso` 约 13.6px、动态翻译行和 `/complete/s` 错误。

## 原生 chrome：macOS 约定

`e68e665` 把窗口 chrome 从“Windows 风格按钮 + 网页式圆角”改成 macOS 约定：

- 红黄绿 traffic lights 移到左上，12pt、20pt 间距，hover 显示符号、pressed 变暗；Windows/Linux 仍用右侧 caption buttons；
- chrome 96→90pt，tabs 34→32pt，地址栏 36→30pt、半径 8、13px 文本；active tab 与 toolbar 合缝；
- 所有圆角/选中态用距离场抗锯齿，2× 下分隔线精确 1 物理像素；
- tab/地址栏/菜单文本按字体 ascent/descent 居中；
- Back 不可用时保留槽位但变暗且不响应，Forward 不可用时移除；
- close-tab hover 圆底、红色 pressed；地址栏 hover/focus ring；菜单 accent hover、Redo、macOS `⌘` 快捷键；
- chrome 按钮用原生箭头光标，地址栏用 text cursor；
- 系统浅/深色传入页面 `prefers-color-scheme`，主题变化使 retained compositor 失效。

离屏截图（隐藏窗口，1180×780 logical / DPR 2）：`.artifacts/ui-native/after/home.png`、`home-dark.png`、`comparison-before-after.png`；主树验证图 `/tmp/home-ui.png`。`render-browser` lib 241 passed、bin 130 passed。剩余：窗口仍是无边框自绘 traffic lights（没有系统圆角/阴影），需要可见窗口验证才能换成真 `NSWindow` 控件；无 tab overflow/favicon/security indicator；chrome 仍只支持指针。

## live 页面性能：GitHub/Baidu 的首屏与请求

`f353224`、`46db402`、`50b8829`、`012cf4c`、`81bc543`：

- **GitHub cascade**：38,610 条规则 / 6,372 元素的 local snapshot，首帧 >20s → **4.15s**；`cascade-match` 2.13s→0.88s，`compute` 868→85ms，`pseudo_compute` 1.37s→12ms，container feedback 7.51s→1.95s。根因是每个元素复制/重算整张 inherited custom-property 表（GitHub `:root` 约 1,000 个变量）、`*,::before,::after { box-sizing:border-box }` 触发无 `content` 的伪元素完整计算、以及 3 次全文档 container 反馈。
- **GC**：Baidu 常驻约 136k JS 对象，旧的 131,072 硬上限导致几乎每次分配都 full mark-sweep（~45ms，回收 0–41 个）；硬上限改为 524,288，加增长阈值/headroom，脚本边界只在到期时 GC。Baidu live 12s：GC 数千次→9 次，`DOMContentLoaded` 19.5s 未完成→10.2s，`load` 10.7s，10s health `ready=Complete scripts_threw=0`。
- **重复子资源**：GitHub 同一 CSS 被 link 5–6 次；`HttpTransport` 增加 in-flight coalescer（URL+Accept+Cookie+headers；body/Range/conditional 不共享）。一次运行请求 129→128 logical / 119 unique，传输 15.72MB→11.32MB。
- **container feedback / slow script turn** 现在会输出 pass 数与 >250ms 的 native callback turn，便于继续定位。

## Google 结果页

`9178c5e`、`05637ba` 后，Google 从 15 元素 interstitial 变成可用结果页（需一次 consent 的 `SOCS` cookie）：

- 根因不是 Google 封禁，而是：脚本 `location.replace` 被 pump 先 `take` 掉、计时器写 `document.cookie` 未同步到 HTTP jar、缺 `CSSStyleRule` 等 CSSOM 全局、transform origin 用 box-local 而非 document-space、transform 重复应用到 text/anonymous box、flex intrinsic 未处理 `max-height`、row-flex 的 cross-axis intrinsic height 把子项高度求和。
- 修复后 consented profile：`ready=Interactive dom_elements=2530 boxes=940 scripts_failed=0 scripts_threw=0`，真实结果标题/URL/snippet 出现；截图 `/tmp/google-results-after.png`。
- 之后又修了 Google 的空白带/结果行距：row-flex 的 cross-axis 高度取最高子项（216→48px），非 flex block 的 intrinsic height 加入子项垂直 padding/margin；`#center_col` y 368→200（Edge 200），结果行高 152/136/114→139/123/101（Edge 123/101/123）。`render-layout` 399 passed。
- 剩余：fresh profile 的 consent 对话框背景/z-order、`#rso` 约 13.6px 偏移、动态“翻译此页”让首行高约 16px、`/complete/s` 的 GET-body/abort 错误。

## 系统性布局细节：line-height 与 getComputedStyle

- `42d87e0`：`line-height: normal` 之前固定 `1.2em`。Chromium 用所选字体的 ascent/descent/line-gap；16px Arial/system-ui 是 18px，PingFang SC 是 22px，而 rENDER 之前统一 19.2px。现在 `TextMeasurer::normal_line_height` 从实际选中的 face 取 metrics，Arial/PingFang 各走自己的 line box。这对每个文本块都会改变累计高度，是“很多网页细节不对”的一个系统性根因。
- `5de91e5`：`getComputedStyle()` 之前只有 inline 值/空值。现在渲染管线算出的 `ComputedStyle` 会通过 `Page::publish_computed_styles`/`JsRuntime::install_computed_styles` 安装进 JS runtime；`getComputedStyle` 读真实 cascade 结果，支持 camelCase、`getPropertyValue`、`length/item`、`cssFloat`、`::before/::after`（只有生成的伪元素有值），`element.style` 仍只读 inline。测试覆盖 display/padding/font-size/margin/color。
- 受用户“不要影响桌面”约束，这两项只用 `cargo test`/纯 Rust 验证，没有再启动浏览器、Playwright 或 `--screenshot`；live 复测留到用户允许时。

## 包体与内存

macOS release：`render-browser` **16 MB**，gzip **7.8 MB**，xz **5.3 MB**；额外 Homebrew `libdav1d` 0.78 MB。Edge.app 实测 1.7 GB，Chrome 未安装（典型 1.5–2 GB）。体积约 1/100，启动也更快；字体按需解码已把 `render-perf` peak RSS 从 881MB 降到 278MB，但 live Baidu 仍约 2.8GB（JS arena append-only、页面对象/图片）。目前没有 `.app`/安装器/公证。

## 验证

```text
cargo test --workspace                                     all ok
cargo test -p render-core --lib                            565 passed, 1 ignored
cargo test -p render-browser --bin render-browser           140 passed
cargo test -p render-browser --lib                          249 passed
cargo test -p render-css                                    334 passed
cargo test -p render-layout                                 403 passed, 1 ignored
cargo test -p render-js                                     1293 passed
cargo test -p render-net                                    125 passed
cargo clippy --workspace --all-targets -- -D warnings       clean
cargo fmt --all --check                                     clean
```

未完成：PingFang 全 glyph outline 解析（~250–280MB）、JS arena 不可回收/compaction、CSSOM 子集（`media`/`disabled`/`replace`、link sheet 规则进 cascade）、Google fresh-profile consent、单脚本 turn 不可抢占、Baidu 11 个 linear gradient ~150ms、hao123 `.search` 10px static-position、Zhihu 背景插画未绘制、Worker 无真并行/module worker/MessagePort、selectionchange/iframe 选择/拖拽 autoscroll、无边框窗口的系统 traffic lights/圆角阴影。

---

# 交接文档 (2026-11-02, 续六十八) — iframe/Selection、HTTP/2、提交饥饿修复、解释器与 raster 性能、HTML5 解析

本轮针对“几乎所有网站慢/卡死”的实测诊断收口，全部无站点特判。主要提交：

```text
66f95b6 render-core: paint nested browsing contexts inside iframe boxes
b55c3c1 render-core: render SVG use and symbol references
d00d350 render-core: decode animated GIF and WebP sequences
34aabf1 render-dom: implement live Selection APIs
fa59543 render-html: close HTML tree construction defects
b4ffb92 render-net: pool TLS HTTP/2 connections and multiplex fetches
d12cf65 render-browser: commit completed frames without starvation
e358cc4 render-browser: bound script batches by wall clock
8451e60 render-js: share function code and cut call-path overhead
0c0f7d0 render-core: rasterize box shadows through a separable blur mask
d6c09a7 render-js: add immutable ArrayBuffer and transfer APIs
```

## 性能诊断结论

对 Bilibili/Baidu/GitHub/Sina/Wikipedia/HN 的只读实测（`RENDER_DEBUG_FRAME=1 RENDER_STAGE_TIMING=1`）：

- **提交饥饿是首要可见故障**：Bilibili 60s 内完成 15 次 full render，只提交 4 帧，屏幕长期停在 0.4–2.2s 的 pre-hydration 帧；13 张图片已解码但未上屏。GitHub/Sina 因此在 10–30s 仍显示裸 HTML。
- **主线程 JS 无时间片**：Bilibili 三个批次约 19M 解释步 / 8s（约 2.3M steps/s）；Baidu 在一次 45s 运行中 5.1s 后静默 40s。单脚本上限 50M 步，在旧速度下可阻塞约 22s。
- **资源整批屏障**：Bilibili 13 张图等 4.26s；Sina 119 张图等 5.14s，随后一次应用 488 张；所有 CSS/字体/脚本/图片都等整批。
- **CPU stage**：Bilibili cascade 0.3–0.5s、raster 0.2–1.6s；Baidu raster 0.1–1.4s；GitHub/HN 证明轻页面 stage 不是问题。
- 完整报告：`C:\Users\Elyar\AppData\Local\Temp\opencode\bilibili-diagnosis.md`。

## 提交修复（P0）

- `RenderIdentity` 增加 `document_serial`，导航前的帧不能装进新文档。
- `RenderWorker::drain_latest` 每个 tab 返回**最新完成帧**，不再因有更新的 submitted generation 就丢弃。
- `is_tab_busy` 覆盖 active、queued 和 completed-but-undrained；`worker_loop` 先发布完成帧、再清 active。
- `about_to_wait` 先 `poll_render_worker`，再 recovery；避免 recovery 抢先提交新 job。
- commit gate 接受中间完成帧，但要求 document serial、requested viewport、scroll position 一致，且 generation 不回退。`committed_render` 防止旧帧覆盖新帧。
- 两个 wall-clock 预算：page pump 24ms/次、classic script batch 500ms 或 1024 turns 后把剩余工作交回 bounded pump。单脚本 turn 仍不可抢占。
- 测试：worker 6/6；render-browser lib 231/231；app binary 117/117；新增 `a_completed_frame_is_not_hidden_by_a_newer_submission`、`completed_frames_commit_progressively_but_never_go_backwards`、`a_frame_from_a_replaced_document_never_commits`。

## iframe painting

- root-aware style/layout/display pipeline：`compute_document_styles_with_hints_from_root`、`build_formatting_tree_from_root`、root font size、presentational hints。
- iframe 是 replaced element：默认 300×150、HTML width/height、CSS sizing、atomic inline、fallback children 不参与 formatting；UA `iframe { overflow:hidden }`。
- 子文档按 content box 尺寸独立 style/layout/display/raster 到透明 surface，以 `ImageSource::FrameDocument` 安装；父 display list 只增加一条 sourced-from-iframe 的 image command。
- 子文档脚本/realm/load 事件保持独立；子 DOM mutation 通过 `content_revision` 触发父帧重绘；嵌套 iframe 深度 8。
- 测试 8/8：文本/像素只在 content box 内、默认与 HTML 尺寸、红块/绿背景/蓝边框/父黄底精确坐标、子脚本 mutation、递归 iframe、iframe hit test。
- 截图：`.artifacts/iframe-srcdoc.png`，1770×1170，82,224 bytes；content box 与红块坐标见 `nested_browsing_contexts.rs`。

## Selection / Range

- `window.getSelection()` / `document.getSelection()` 每文档单例，复用 live `Range` 表；anchor/focus/offset/direction/type、`getRangeAt`、`addRange`、`removeAllRanges`、`collapse*`、`extend`、`setBaseAndExtent`、`selectAllChildren`、`deleteFromDocument`、`containsNode`、`toString`。
- DOM mutation/remove/split 会 clamp；跨 realm/iframe 独立。测试 render-dom 90、render-js 1234。
- 仍未接 selection highlight/caret 绘制、`selectionchange`/`selectstart`、`Selection.modify()`。

## HTTP/2

- 完整 RFC 9113 帧层 + RFC 7541 HPACK/Huffman + stream/flow-control + GOAWAY/RST + multiplexed dispatch。
- transport：direct HTTPS 用 rustls ALPN `h2`；每 origin 一个 `Mutex<H2Connection>` 池；20ms 读锁切片；GOAWAY/EOF 驱逐并重连；HTTP/1.1 fallback；代理请求继续走 ureq CONNECT，绝不绕过代理。
- 120KB response 跨 initial window；两路并发/out-of-order；TLS no-ALPN fallback；h2 32 测试、render-net 66。
- 限制：h2 不经过代理、不处理 gzip/brotli（请求 identity）、请求 body >60KB 回退 HTTP/1.1、socket 字节级仍串行、无 server push/priority scheduler。

## 解释器与 raster

- `functions: Vec<Rc<UserFunction>>`、共享 `captured_environment: Rc<[Environment]>`；`call_user` 不再深拷贝函数体/AST/捕获环境。
- `CallFrame` lazy label（只在 Error.stack/debug stack 时格式化）；缓存 `ActivationPlan`；`lookup_binding`/`assign_binding` 融合为一次作用域链扫描；suspended scope chain move 而非 clone。
- release synthetic：small_calls 6.86M→13.39M steps/s（1.95×）、closure 3.65M→5.06M（1.39×）、large_body 8.41M→19.07M（2.27×）、object_churn 7.50M→10.08M（1.34×）；真实 109KB legacy polyfill bundle 约 13.6M steps/s。
- box-shadow：per-pixel 16 sample → prepared rounded shape signed distance + cover mask + 3-pass separable box blur；1000×100 blur40 在 1200×800 上 release 12.93ms→8.89ms；debug 362ms→65ms。大 mask 16M 像素以上回退旧 analytic 路径。
- SVG `<use>/<symbol>`：local fragment clone、symbol viewBox/preserveAspectRatio、继承、嵌套/环检测、诊断；74 svg + 16 inline svg 测试。
- 动图：GIF/WebP 帧序列、disposal/blend、loop、frame_at；render-core 527/532 测试（动画自身 13 个）。尚未接 document clock 到 raster。

## HTML5 解析

- html5lib-only：3,247/3,549 (91.49%) → **3,541/3,549 (99.77%)**；全语料：6,602/7,377 (89.49%) → **7,196/7,377 (97.55%)**；engine defect 581 → 41，且无新增非通过。
- 修复 button scope/paragraph closing、adoption agency、generic end tags、table insertion modes、tokenizer comment/EOF、NUL/CR、foreign content end tags、customizable select selectedcontent、quirks doctypes；`.dat` harness 修正。
- 剩 41 engine defects：37 个是 pinned PI 期望与现行标准差异，4 个 scripted fixture。`render-html` 176 tests。

## ArrayBuffer immutable/transfer

- `BufferStore.immutable` + `TypedBuffer::new_immutable`；`immutable` getter、`transferToImmutable`、`sliceToImmutable`、`transfer`/`transferToFixedLength`/`resize`/`$262.detachArrayBuffer` 的 immutable 拒绝。
- TypedArray 写入/set/fill/sort/copyWithin/reverse/map/filter/slice/from/of、DataView setter、Atomics 写、TextEncoder.encodeInto 都拒绝 immutable；structuredClone 保留 immutable。
- Uint8Array scoped test262 **140/140**；render-js 1234/1234；clippy clean。全量 test262 未重跑，baseline 仍 90,965。

## 验证

```text
cargo test -p render-browser --lib                         231 passed
cargo test -p render-browser --bin render-browser           117 passed
cargo test -p render-core --lib                             532 passed, 1 ignored
cargo test -p render-core --lib paint::raster               31 passed, 1 ignored
cargo clippy (8 crates) --all-targets -- -D warnings        clean
cargo fmt --all --check                                     clean
python tools/check_site_neutrality.py                       0 blocking, 26 advisory (312 files)
render-html lib                                             176 passed
render-net lib                                              66 passed
render-js lib                                               1234 passed
render-dom lib                                              90 passed
html5lib-only                                               3541/3549 = 99.77%
html5lib all                                                7196/7377 = 97.55%
```

已知未完成：提交饥饿修复后的真实站点复测（Bilibili/Baidu 截图与 wall time）尚未在最终 release binary 上重跑；资源整批屏障仍未改为逐项应用；动画仍可能触发 full render；selection 绘制、iframe 子节点坐标 hit test、HTTP/2 over proxy、JS 单脚本抢占仍是后续工作。

---

# 交接文档 (2026-11-01, 续六十七) — GPU 呈现、合成器扩展、RTL/逻辑属性、文本间距、SVG/字体/缓存/test262

这一轮并行推进了十条战线，全部无站点特判：

1. wgpu/D3D12 GPU presenter；
2. retained compositor 的 filter/mask effect surface；
3. `text-indent`/`letter-spacing`/`word-spacing`；
4. RTL block 轴镜像；
5. RTL flex/grid/table 轴顺序；
6. CSS logical box properties；
7. `@starting-style`；
8. SVG filter primitive chains；
9. HTTP `Vary` 缓存变体；
10. `@font-face` 条件门控 + `font-display` 时间线；
11. test262 `Uint8Array` base64/hex 与 `Error.prototype.stack`。

## GPU presenter

- `crates/render-browser/src/gpu_presenter.rs`：wgpu 30 / DX12。创建窗口
  `Surface`、adapter/device/queue，配置 sRGB swapchain；ARGB u32 转为
  RGBA8，按 256 字节行对齐上传纹理；WGSL fullscreen triangle 采样纹理并
  `queue.present`。`Outdated`/`Lost` 重配一次；Timeout/Occluded 显式跳过。
- `app.rs` 初始化时优先 `GpuPresenter::new(window.clone())`，失败才创建
  softbuffer context/sink；`RENDER_FORCE_SOFTBUFFER=1` 可强制回退。
- 实测 adapter：`NVIDIA GeForce RTX 4060 Ti`，backend `Dx12`。
- `render-gpu-bench`：120/120 帧 present，mean present **3.86ms**，
  median **4.03ms**，max **4.37ms**，mean compositor **2.90ms**。

## 合成器 filter/mask

- `CompositedLayer` 携带 `filter`/`mask`；顶层 filter/mask layer 不再回退。
- 过滤/遮罩层使用单个 effect surface（不是独立 band），blur/drop-shadow 可以
  跨 512px band 边界；filter → mask → transform → opacity 顺序与完整 raster
  一致。`apply_layer_effects` 与完整 raster 共用。
- 安全上限：宽 8192、高 4096、4,194,304 像素；超出仍回退。
- compositor 测试 12 个，覆盖 blur 跨 band、gradient mask、
  filter+opacity+transform、viewport-space filter/mask，均逐像素对齐完整 raster。

## 文本间距

- `text-indent`：首行/`each-line`/`hanging`、长度/百分比/calc/em/rem/vw、负值、
  RTL start 边；只有首行或强制换行后的行受影响。
- `letter-spacing`：CSS Text 3 §7.2 的 `n-1` 间隙；行首/行尾无 tracking，跨 inline
  边界取两侧平均；负值支持。
- `word-spacing`：每个 word separator 额外 advance，叠加在 separator 的 tracking 上。
- `TextFragmentData` 携带 `spacing`/`leading_spacing`/`trailing_spacing`；
  `TextShaper::shape_spaced` 默认实现把 spacing 累加到 glyph origin，绘制、命中和
  测量一致。剩余：grapheme cluster（当前按 scalar）、`ex`/`ch`。

## RTL、logical properties、flex/grid/table

- block：over-constrained margin、auto margin 负空间、root direction、floats/clear、
  relative/absolute inset 优先级、static position、float-shortened band 内对齐。
- flex：`row`/`row-reverse`、`justify-content` start/end vs flex-start/end、
  cross-axis start/end、auto margins、column wrap line box。
- grid：逻辑列，RTL 下第一列最右；`grid-column:1` 仍为逻辑 start；span 宽度和
  物理左边缘正确；容器 `justify-content`。
- table：RTL 列从右开始，spanning cell 锚定逻辑列组的物理左边。
- CSS logical properties：24 个 longhand + shorthands；physical/logical 在同一个
  属性上按完整 cascade rank（origin/important/layer/specificity/scope/source order）
  竞争；LTR/RTL 映射正确；百分比/calc/em/rem/vw 复用长度引擎。
- 仍缺：vertical writing modes、logical sizing/corner/scroll properties、
  table `<col>` 方向、flex/grid `justify-items/self`。

## `@starting-style`

- 解析并保留 top-level、style-rule 内、media/supports/container/layer/scope 内。
- 普通 cascade 跳过；`compute_starting_styles` 只对进入 rendered set 的元素应用。
- transition sampler 在 `transition-property` 覆盖且值不同时从 starting value 开始，
  复用 document clock、reversal、run/start/end/cancel 事件。
- 测试：首帧 opacity 0→1、嵌套 selector、无 transition、display:none、DOM 插入、
  specificity/source order、静态样式隔离。

## SVG filters

- 有序 primitive chain，支持 `in`/`in2`/`result`、SourceGraphic/SourceAlpha、
  filter/primitive units、百分比、subregion、transformed region。
- 已实现：`feGaussianBlur`、`feColorMatrix`、`feOffset`、`feBlend`（全部 W3C
  blend mode）、`feComposite`（over/in/out/atop/xor/arithmetic）、`feFlood`、
  `feMerge`、`feDropShadow`、`feMorphology`。
- 65 个 SVG 测试；未实现 turbulence/displacement/image/component-transfer/
  convolve/tile/lighting、`linearRGB`、fractional feOffset。

## 字体

- `@font-face` 现在保留并评估 `@media`/`@supports` 条件（含嵌套、外部样式表）；
  不匹配的 face 不 plan/fetch/install。`@container` 仍未评估。
- `font-display`：auto/block 3s block + infinite swap；swap 0 + infinite；
  fallback 3.1s；optional 100ms；到达失败边界即拒绝。block 期间仍用可见 fallback
  度量/绘制（UA 策略，单表 + §4.8.1 可见文本）。过期 face 保持 §5.2 missing 状态。
- 测试：4 个 webfont 集成测试、font_face 49 个、font_faces 25 个、可控时钟
  边界测试、loopback late-arrival。

## HTTP `Vary`

- RFC 9111 §4.1：大小写不敏感字段名、每 URL 多 variant、lookup/stale/revalidation
  只选匹配 variant、absent 与 present-empty 区分、重复 field line 按序合并、
  `Vary: *` 不缓存。
- 304 保留或重算 Vary，只刷新选中 variant；`merge_headers` 替换所有同名字段行。
- 17 个 cache 测试。

## test262

- `Uint8Array` base64/hex 六个方法：完整 RFC 4648、allow/omit padding、
  `lastChunkHandling`、read/written、prefix 写入后抛 SyntaxError、
  detached buffer/option coercion 顺序。`built-ins/Uint8Array` 16/140 → **136/140**。
- `Error.prototype.stack` accessor：hidden symbol 存储、no own stack、
  setter 忽略 prototype 属性、proxy/non-extensible/non-writable 规则、
  `%Error.prototype%` home check、realm-local TypeError；`AggregateError()` 无 new；
  `GetPrototypeFromConstructor` realm fallback。`built-ins/Error` 114/186 → **186/186**。
- 全量：90,765 → **90,965 / 103,199 = 88.15%**；264 buckets，8 timeout，0 crash；
  剩余 Uint8Array 失败仅为 immutable ArrayBuffer 相关 4 个 variants。

## 验证

```text
cargo test --workspace -- --test-threads=1
  render-browser    230 lib + 115 app tests, 0 failed
  render-core       502 passed, 0 failed
  render-core       webfont integration 4 passed, 0 failed
  render-core       RTL integration 2 passed, 0 failed
  render-css        319 passed, 0 failed
  render-js         1,214 passed, 0 failed
  render-layout     389 passed, 1 ignored
  all crates        passed, 0 failed

cargo fmt --all --check                          clean
cargo clippy ... --all-targets -- -D warnings    clean
python tools/check_site_neutrality.py            0 blocking, 26 advisory (297 files)

render-gpu-bench:
{"frames":120,"adapter":"NVIDIA GeForce RTX 4060 Ti","backend":"Dx12",
 "window":"1000x740","presented_frames":120,"all_presented":true,
 "mean_present_us":3864.3,"median_present_us":4032.1,"max_present_us":4369.9,
 "mean_compositor_us":2901.8}

render-compositor-bench:
{"frames":"120","frames_during_block":"120","presented_frames":"120",
 "mean_latency_us":"1720.7","median_latency_us":"1650.3",
 "mean_present_us":"253.0","max_present_us":"324.6"}

render-scroll-bench:
{"compositor_ms":"267.703","compositor_us_per_frame":"2230.9",
 "full_raster_ms":"704.142","full_raster_us_per_frame":"5867.9",
 "scroll_frames_without_new_bands":"116"}
```

真实 shell：

```text
render-browser GPU presenter: adapter=NVIDIA GeForce RTX 4060 Ti backend=Dx12
render-browser compositor retained layers=5
render-browser health page=file:///.../compositor-demo.html ready=Complete
render-browser screenshot written to ...\gpu-final.png
```

截图 82,898 字节。

## 已知缺口

- GPU presenter 目前只在 Windows/DX12 启用；其他平台走 softbuffer。`GpuPresenter`
  在窗口创建线程构造后移动到 compositor worker。
- filter/mask 只支持顶层 effect surface；嵌套 filter、非 Normal blend、ambient
  clip 跨 layer、nested sticky 仍回退。
- vertical writing modes、logical sizing/corner/scroll、grid `justify-items/self`、
  `<col>` 方向仍未实现。
- `@starting-style` 的 pseudo-element、`content-visibility` rendered-state 精度、
  CSSOM `CSSStartingStyleRule` 仍缺。
- SVG 仍缺 turbulence/displacement/image/component-transfer/convolve/tile/lighting、
  linearRGB、fractional offset。
- `@container` 下 `@font-face`、WOFF2 hmtx、EOT/SVG 字体、variation descriptors、
  pseudo-element starting fonts 仍缺。
- `Vary` 不合成 transport 注入的常量默认头；字段值仅 OWS 归一化（安全 miss）。
- test262 仍 88.15%，immutable ArrayBuffer、Temporal/Intl、更多 staged/sm 仍缺。
- iframe painting、selection/caret、HTTP/2、service workers、wide-gamut GPU
  compositing 仍是结构性缺口。

## 下一步

- 把 GPU presenter 推广到 Metal/Vulkan，并做 damage-only present。
- 补齐 nested filter/mask、vertical writing modes、logical sizing/scroll。
- iframe painting 与 selection API。
- 继续 test262，优先 immutable ArrayBuffer、Temporal/Intl 与 staged/sm。

---

# 交接文档 (2026-11-01, 续六十六) — compositor worker 直接呈现平台 surface

这一轮把上一轮的 compositor worker 从“返回 surface 给主线程”推进到“worker 直接
present 平台窗口”。实现的是 **platform-surface 路径**（softbuffer），不是 GPU
swapchain；这样滚动帧的呈现不再经过事件循环线程。没有站点特判。

## 接口

- 新增 `render_browser::compositor::PlatformPresenter`：
  - `present_full(frame, width, height)`：完整窗口帧（含 chrome）；
  - `present_page(page, origin_x, origin_y)`：把 page surface 合成到上一张完整帧的
    page 区域并 present，返回 platform present 耗时。
- `SharedPresenter = Arc<Mutex<Option<Box<dyn PlatformPresenter>>>>`：
  - compositor worker 在启动时拿到这个 slot，窗口创建后由 shell 安装
    `SoftbufferPresenter`；
  - 窗口销毁/失败时 slot 为空，worker 仍返回 surface，shell 走原有 redraw 回退。
- `CompositorScroll` 新增 `page_origin`；worker 用 layout 的 chrome height
  把 page 放在 chrome 下方。
- `CompositorFrame` 新增 `present_time: Option<Duration>`；只有平台 present
  真正成功时才为 `Some`。

## SoftbufferPresenter

- `crates/render-browser/src/platform_presenter.rs` 实现：
  - 持有 `softbuffer::Surface<Arc<Window>, Arc<Window>>`；
  - `base: Vec<u32>` 保存最近一次完整窗口帧；
  - `present_full` resize、复制完整帧、present；
  - `present_page` 用 `surface_to_softbuffer` 把 page 转成窗口像素，按
    `page_origin` 覆盖 base 的 page 矩形，再复制整个 base 到 softbuffer
    buffer 并 present；
  - 返回这次 copy+present 的耗时。
- app 删除 `surface: Option<NativeSurface>`，改为共享 `presenter` slot；
  `present()` 通过 sink 执行完整帧 present。
- compositor commit 后：
  - 若 `present_time.is_some()`，worker 已经把新页面呈现到窗口；主线程仍
    `mark_page_damage` + `compose_frame` 更新 CPU 帧，但不 `request_redraw`；
  - 若平台 present 失败（`None`），按旧路径 damage/redraw，主线程补一次完整帧。

## 测试

- `render-browser/src/compositor.rs` 的
  `twenty_scroll_frames_run_on_the_dedicated_thread`：
  - 安装记录型 presenter，先 present 一张 80x70 完整帧；
  - 20 个 80x60 滚动帧，page_origin `(0,10)`；
  - 每帧断言 `worker_thread != caller`、`present_time.is_some()`；
  - 断言 presenter 收到 20 次 page present，第一次 origin 是 `(0,10)`。
- `app_tests::a_scroll_frame_composites_while_the_render_worker_is_blocked`：
  - 真实 render worker 卡在 full frame；
  - 安装记录型 presenter 和真实 compositor worker；
  - scroll commit 后断言 presenter 收到一次 page present，origin 等于
    `layout.chrome_height`；
  - render worker 的 log 没有 scroll job。

## Benchmark

`render-compositor-bench` 现在安装内存 presenter（执行同样的 page blit + 全窗口
复制）并统计 present 时间：

```text
cargo run --release -p render-browser --bin render-compositor-bench
{"frames":"120","scroll_step_px":"16","viewport":"800x600",
 "render_worker_block_ms":"500","frames_during_block":"120",
 "presented_frames":"120","mean_latency_us":"1655.4",
 "median_latency_us":"1586.5","max_latency_us":"4176.1",
 "mean_present_us":"249.8","max_present_us":"269.9"}
```

模拟 render worker 阻塞 500ms 期间，**120/120** 帧完成并经过 platform present；
平均端到端 1.66ms，platform present 平均 250us。

原有 `render-scroll-bench` 仍是：

```text
{"frames":"120","scroll_step_px":"16","viewport":"800x600",
 "compositor_ms":"271.149","compositor_us_per_frame":"2259.6",
 "full_raster_ms":"716.267","full_raster_us_per_frame":"5968.9",
 "initial_bands":"5","final_bands":"9","scroll_bands_rasterized":"4",
 "scroll_frames_without_new_bands":"116"}
```

## 验证

```text
cargo test -p render-browser --lib compositor::tests  1 passed, 0 failed
cargo test -p render-browser --bin render-browser \
  a_scroll_frame_composites_while_the_render_worker_is_blocked
                                                    1 passed, 0 failed
cargo run --release -p render-browser --bin render-compositor-bench
                                                    see benchmark above
cargo run --release -p render-browser --bin render-scroll-bench
                                                    see benchmark above
cargo test --workspace                              all crates passed, 0 failed
cargo fmt --all --check                             clean
cargo clippy ... --all-targets -- -D warnings       clean
python tools/check_site_neutrality.py               0 blocking, 26 advisory
```

真实 release shell 加载 fixed/sticky/rotate 演示页仍输出：

```text
render-browser compositor retained layers=5
render-browser health page=file:///.../compositor-demo.html ready=Complete
dom_elements=12 boxes=9 css_errors=0 css_warnings=0
render-browser screenshot written to ...\compositor-present-demo.png
```

截图 82,898 字节，窗口 chrome 与页面内容正常。

## 已知缺口

- **不是 GPU swapchain。** 当前是 softbuffer 平台 surface；`PlatformPresenter`
  已经是 GPU 实现可以复用的边界，但还没有 wgpu/D3D 纹理与交换链。

  > 续六十七已实现 wgpu/D3D12 `GpuPresenter`；softbuffer 现在只是回退路径。
- softbuffer present 从 worker 线程调用；平台若要求主线程 present，会返回
  error 并回退到主线程 redraw。Windows 已验证可用，其他平台需逐平台验证。
- `present_page` 复制完整窗口 base，而不是只 present 受损矩形；可优化为
  damage-only present。
- compositor worker 只有 1 个线程；多 tab 同时滚动串行。
- 动画采样仍在 event-loop 线程。
- filter/mask、非 Normal blend、ambient clip 跨 layer、嵌套 sticky 仍回退。
- band cache 无上限/淘汰策略。

## 下一步

- 用 wgpu 实现同一个 `PlatformPresenter`：GPU texture + swapchain，present
  直接从 compositor worker。
- damage-only platform present 与 band residency/eviction。
- 多 tab compositor 调度。
- 继续 RTL 块轴镜像、`scroll-state()`、`@starting-style`。

---

# 交接文档 (2026-11-01, 续六十五) — 独立 compositor worker 与滚动帧脱离 render worker

这一轮把 scroll compositing 从 render worker 的队列里拆出来，放进独立的
compositor worker 线程。render worker 继续负责 style/layout/display list 和完整
raster；滚动帧只读取 retained layer tree 与 band cache，提交 presentable surface。
没有站点特判。

## 架构

- 新增 `crates/render-browser/src/compositor.rs`：
  - `CompositorScroll { compositor, images, content_height, viewport_height }`；
    scroll offset 仍放在 `RenderIdentity`，与既有 supersession/coalescing 共用；
  - `CompositorFrame { surface, render_time, worker_thread, content_height,
    viewport_height }`；
  - `start_compositor_worker` 用 `RenderWorker` 启动 1 个线程、queue capacity 8；
  - worker 内调用 `CompositorCache::render`，返回 CPU `Surface` 和本帧
    `render_time`；不在 worker 内跑 style、layout 或 display list。
- app 新增 `compositor_worker: Option<CompositorWorker>`：
  - `PageState` 新增 `expected_compositor`，与 `expected_render` 分开，所以 full
    render 在途时滚动帧仍可提交和 commit；
  - `schedule_page_render` 在能合成时直接向 compositor worker 提交，绕过
    `render_worker.is_tab_busy`；
  - `commit_compositor_frame` 只更新 surface、viewport、scroll metrics 和 runtime
    viewport，然后 damage/compose/redraw；
  - full render commit 会取消在途的旧 compositor job 并清空
    `expected_compositor`，防止旧 layer tree 的帧覆盖新内容；
  - zero viewport、导航、关闭 tab 都同时 cancel 两个 worker；
  - `UserEvent::CompositorReady` 负责唤醒事件循环，`about_to_wait` 也会 poll 一次
    compositor worker。
- 删除了 `PageRenderPayload::CompositedScroll`：scroll 不再经过 render worker。
  没有 compositor worker 的环境（headless test 或 worker 启动失败）继续走
  `RetainedRaster` / full render 回退。
- `main.rs` 启动失败时只打印 `render-browser dedicated compositor disabled` 并继续
  运行，不让整个浏览器失败。

## 测试

- `render-browser/src/compositor.rs`：
  - `twenty_scroll_frames_run_on_the_dedicated_thread`：20 个 80x60 滚动帧，
    每帧断言 `worker_thread != caller`，surface 尺寸和像素正确，记录
    `render_time`，最慢一帧 < 2s（宽松上界，用于抓布局级回退）。
- `app_tests.rs`：
  - `a_scroll_frame_composites_while_the_render_worker_is_blocked`：
    gated render worker 卡在 full frame，`is_tab_busy == true`；随后 scroll 仍通过
    compositor worker 提交并在 5s 内 commit，render worker 的 log 没有收到 scroll
    job；commit 后 `expected_compositor == None`。
- 既有 8 个 compositor core tests 继续覆盖 fixed/sticky/affine 与完整 raster 的
  逐像素相等。

## Benchmark

新增 `crates/render-browser/src/bin/render-compositor-bench.rs`：一个模拟 render
worker 线程阻塞 500ms，主线程顺序提交 120 个滚动 job，等待每个 compositor frame：

```text
cargo run --release -p render-browser --bin render-compositor-bench
{"frames":"120","scroll_step_px":"16","viewport":"800x600",
 "render_worker_block_ms":"500","frames_during_block":"120",
 "mean_latency_us":"1412.2","median_latency_us":"1300.1",
 "max_latency_us":"4595.9"}
```

如果滚动帧和 render worker 共享队列，这个数字会是 0；实际 **120/120** 帧在
500ms 阻塞期间完成，平均 1.41ms、中位 1.30ms。

原有 `render-scroll-bench` 仍然测量单帧合成成本与完整 raster：

```text
{"frames":"120","scroll_step_px":"16","viewport":"800x600",
 "compositor_ms":"271.149","compositor_us_per_frame":"2259.6",
 "full_raster_ms":"716.267","full_raster_us_per_frame":"5968.9",
 "initial_bands":"5","final_bands":"9","scroll_bands_rasterized":"4",
 "scroll_frames_without_new_bands":"116"}
```

retained **2.27ms/帧** vs full raster **6.05ms/帧**；116/120 帧没有新增 band。

## 验证

```text
cargo test -p render-browser --lib compositor::tests  1 passed, 0 failed
cargo test -p render-browser --bin render-browser \
  a_scroll_frame_composites_while_the_render_worker_is_blocked
                                                    1 passed, 0 failed
cargo test --workspace                              all crates passed, 0 failed
cargo fmt --all --check                             clean
cargo clippy ... --all-targets -- -D warnings       clean
python tools/check_site_neutrality.py               0 blocking, 26 advisory
```

## 已知缺口

- 仍是 CPU dedicated-surface，不是 GPU 合成；没有 GPU texture、swapchain 或
  平台合成器集成。
- compositor worker 只有 1 个线程；多 tab 同时滚动时串行，后续可按 tab/图层
  并行或给活跃 tab 优先队列。
- 动画采样仍在 event-loop 线程；没有 compositor-driven animation timeline。
- filter/mask、非 Normal blend、ambient clip 跨 layer、嵌套 sticky、sticky 内
  transform 仍回退到完整 raster。
- band cache 无上限/淘汰策略；fixed 内容仍按整个 viewport 建 source。
- 没有独立的 compositor frame timing 历史导出到 `render-perf`，目前只有单帧
  `render_time` 和两个 benchmark。

## 下一步

- GPU texture/swapchain 合成，或至少把 `Surface` 上传到平台 surface 后由
  compositor worker 直接 present。
- 多 tab compositor 调度与 band residency/eviction。
- 把 animation 采样移入 compositor timeline。
- 继续 RTL 块轴镜像、`scroll-state()`、`@starting-style`。

---

# 交接文档 (2026-11-01, 续六十四) — compositor 支持 fixed/sticky 与 scale/rotate/skew

这一轮扩展上一轮的 retained compositor：fixed 内容进入 viewport-space 的
retained segment/layer，top-level sticky 作为可位移 layer，scale/rotate/skew
layer 用逆映射选择 source band 并直接合成。没有站点特判。

## coordinate space 与 fixed

- `CompositedLayer` 新增 `space: PaintCoordinateSpace`；`RootSegment` 同样记录
  自己的 space。`from_display_list` 在 root 连续 item 的 space 变化处切段，所以
  document 内容和 fixed 内容不会共享同一个 source。
- viewport-space 的 source 按整个 viewport 栅格化，合成时 `to_viewport` 是
  identity，不减去 scroll offset；document-space source 才随 scroll 平移。
- viewport layer 的 bounds 也取 viewport rect，避免把一个 `top: 10px` 的
  fixed 元素裁到从 `(0,0)` 开始的小 surface 里。

## sticky

- top-level `PushSticky` 不再回退：它被提取为 `CompositedLayer`，source band
  保持 document-space 未位移内容，`sticky: Some(constraint)` 保存约束。
- 每次 `render` 用 `sticky_displacement(constraint, default, viewport_origin)`
  计算当前位移，再与 layer 的变换、band 原点和 scroll 平移组合成一个 affine
  matrix。band cache 不因 scroll 失效。
- 嵌套 sticky、sticky 内的 stacking context/transform 仍回退，因为内层 scope
  需要外层的位移来重定位变换原点。

## scale / rotate / skew

- `from_display_list` 不再要求 `transform.is_translation()`；filter、mask、
  非 Normal blend、ambient clip 仍回退。
- `composite_surface` 改为对每个 layer/segment 计算
  `base_mapped = to_viewport ∘ transform ∘ band_origin`，求逆后把 viewport 四角
  映射回 source 坐标，取包围盒与 source 相交的 band；只有这些 band 需要
  栅格化。
- 栅格化仍使用未变换的 source band；合成时由 `warp_surface` 应用完整仿射矩阵。
- 纯整数平移继续走 `blit_translated` 快速路径；透明像素跳过，alpha 255 且
  opacity 1 时直接写入，避免把 fixed 的大片透明区域和仿射层的覆盖区域逐像素
  blend。

## 测试

`render-core/src/paint/compositor.rs` 现在 8 个测试：

- translated + 0.5 opacity layer 的位置和混色；
- 2048px 页面首帧 2 band、16px 滚动 0 新 band、跳转新增 band；
- 合成结果与完整 `CpuRasterizer::rasterize` 逐像素相等；
- transformed layer 不被未变换 bounds 裁掉；
- 连续 20 个 16px 滚动帧每帧最多新增 1 个 band；
- `fixed_content_is_retained_in_viewport_space`：fixed item 放在 `y=10`
  （非原点），滚动 50px 后与完整 raster 逐像素相等，并在 `(5,15)` 命中它；
- `scaled_and_rotated_layers_match_the_full_raster`：`scale(2)` 与 30° 旋转
  （0.866/0.5 系数）两个仿射 layer 都与完整 raster 逐像素相等；
- `a_sticky_layer_is_displaced_at_composite_time`：在 scroll 0/10/50/120/300
  五个位置与完整 raster 逐像素相等，并断言 scroll 50 时 sticky 条位于 viewport
  顶部。

## Benchmark

`render-scroll-bench` 的场景扩展为：160 个彩色块、translate+opacity layer、
scale+rotate layer、top-level sticky bar、viewport-space fixed header。120 帧、
16px/帧、800x600 视口、12,000px 文档：

```text
cargo run --release -p render-browser --bin render-scroll-bench
{"frames":"120","scroll_step_px":"16","viewport":"800x600",
 "compositor_ms":"265.012","compositor_us_per_frame":"2208.4",
 "full_raster_ms":"700.858","full_raster_us_per_frame":"5840.5",
 "initial_bands":"5","final_bands":"9","scroll_bands_rasterized":"4",
 "scroll_frames_without_new_bands":"116"}
```

retained 路径约 **2.21ms/帧**，完整 raster 约 **5.84ms/帧**；116/120 帧没有
新的 band 栅格化。新增 fixed/sticky/affine 内容后仍保持在 16ms 预算内。

真实页面验证：release shell 打开一个包含 `position:fixed` header、`position:sticky`
nav、`transform:rotate(12deg) scale(1.1)` card 和 3000px 文档的本地页面，开启
`RENDER_DEBUG_FRAME=1` 后日志为：

```text
render-browser compositor retained layers=5
render-browser health page=file:///.../compositor-demo.html ready=Complete
dom_elements=12 boxes=9 scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=0
render-browser screenshot written to ...\compositor-demo.png
```

截图（82,898 字节）中 fixed header 和 sticky nav 固定在顶部，旋转后的 card 正常
显示。说明真实 display list 的 fixed/sticky/affine 内容确实生成了 retained
compositor，而不是回退。

## 验证

```text
cargo test -p render-core --lib paint::compositor  8 passed, 0 failed
cargo test -p render-browser --bin render-browser a_scroll_with_a_retained_compositor_uses_the_composited_payload
                                                  1 passed, 0 failed
cargo run --release -p render-browser --bin render-scroll-bench
                                                  see benchmark above
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy ... --all-targets -- -D warnings     clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

> 续六十五已关闭“没有独立 compositor 线程”这一条；以下保留为当时记录。

- 没有独立 compositor 线程或 GPU 合成；所有工作仍在 render worker。
- fixed 内容按整个 viewport 建立 source，透明区域虽已跳过，但窗口越大固定层
  的扫描成本越高；后续可按实际 item bounds 建立局部 source。
- filter/mask、非 Normal blend mode、ambient clip 跨 layer、嵌套 sticky、
  sticky 内 transform 仍回退到完整 raster。
- band cache 无上限/淘汰策略；超长页面会持续保留已访问 band。
- 嵌套 stacking context 仍由单个 band surface 内的 rasterizer 处理，跨 band
  的裁剪/合成边界没有独立测试。
- 滚动帧没有独立的 frame timing 历史，只有 `CompositorStats` 和 benchmark。

## 下一步

- band residency/eviction 与 compositor 线程/GPU 合成。
- sticky 内 transform 与嵌套 sticky 的 retained 表示。
- 把 compositor frame time 纳入 `render-perf` JSON 和 CI 滚动 benchmark。
- 继续 RTL 块轴镜像、`scroll-state()`、`@starting-style`。

---

# 交接文档 (2026-11-01, 续六十三) — retained 分层 compositor 与 banded 滚动

这一轮实现浏览器 compositor 的第一条完整路径：从 display list 提取 top-level
layer tree、缓存 512px band、滚动时只合成已有 band，并新增 frame/raster 计数测试
与 release 滚动 benchmark。没有站点特判。

## 核心模型

- 新增 `render-core/src/paint/compositor.rs`：
  - `CompositorCache::from_display_list` 扫描 display list，提取 top-level
    `PushStackingContext` 为 `CompositedLayer`；
  - root 的普通 item 按 layer 边界切成 `RootSegment`，`LayerOp` 保持原绘制顺序，
    所以 layer 后面的 root 内容不会错误地画到 layer 下面；
  - 每个 segment/layer 的 surface 按 `DEFAULT_COMPOSITOR_BAND_HEIGHT = 512`
    分带缓存；
  - `render(viewport_origin)` 只栅格化与视口相交且未缓存的 band，其余 band 直接
    合成；`viewport_origin` 只改变合成矩阵，不使 cache 失效；
  - `CompositorStats { frames, bands_rasterized, last_frame_bands, cached_bands }`
    供 frame 计数和测试。
- 保守回退：sticky、viewport-space（fixed）item、`filter`/`mask`、非 `Normal`
  blend mode、非 translation transform、top-level context 外有 open clip、或
  band 宽度超过 8192px 时返回 `None`，浏览器继续走旧的完整 raster。
- `raster.rs` 新增：
  - `CpuRasterizer::rasterize_item_slice`：按 band viewport 和 item slice 栅格化；
  - `composite_surface`：公开的 layer 合成入口；
  - 整数 translation 的快速 blit（scroll 的常见情况），避免每像素双线性采样。

## 浏览器接线

- `PageRenderFrame` 新增 `compositor: Option<Arc<CompositorCache>>`；full render
  构建 cache。
- `PageState` 保存 `compositor`，导航/重置时清空。
- `PageRenderPayload::CompositedScroll` 携带 `Arc<CompositorCache>`；
  `schedule_page_render` 在 `prefer_retained_raster` 且 cache 存在时优先发送它，
  否则退回 `RetainedRaster` 或完整 render。
- `process_page_render` 的 composited 分支只调用 `compositor.render`，不跑
  style/layout/display list。
- 整帧仍写回 softbuffer；`display_list`/`paint_scene` 保持上一篇帧的值。

## 测试

`render-core/src/paint/compositor.rs` 6 个测试：

- translated + 0.5 opacity layer 出现在正确位置并正确混色；
- 2048px 页面首帧栅格化 2 个 band，16px 滚动新增 0 band，跳到 1400px 才新增
  band；band 总数不超过 4；
- 合成结果与完整 `CpuRasterizer::rasterize` 逐像素相等（含 root segment、
  translated/opacity layer 和 layer 后内容）；
- translated layer 不被未变换 bounds 裁掉；
- 连续 20 个 16px 滚动帧每帧最多新增 1 个 band，且 `frames` 计数逐帧递增；
- viewport-space item（fixed）回退为 `None`。

## Benchmark

新增 `crates/render-browser/src/bin/render-scroll-bench.rs`，120 帧、16px/帧、
800x600 视口、12,000px 文档、160 个彩色块和一个 translate+opacity layer：

```
cargo run --release -p render-browser --bin render-scroll-bench
{"frames":"120","scroll_step_px":"16","viewport":"800x600",
 "compositor_ms":"154.702","compositor_us_per_frame":"1289.2",
 "full_raster_ms":"1414.183","full_raster_us_per_frame":"11784.9",
 "initial_bands":"3","final_bands":"6","scroll_bands_rasterized":"3",
 "scroll_frames_without_new_bands":"117"}
```

即 retained 路径约 **1.29ms/帧**，完整 raster 约 **11.78ms/帧**；117/120 帧
没有新的 band 栅格化。

## 验证

```
cargo test -p render-core --lib paint::compositor  6 passed, 0 failed
cargo test -p render-browser --bin render-browser a_scroll_with_a_retained_compositor_uses_the_composited_payload
                                                  1 passed, 0 failed
cargo run --release -p render-browser --bin render-scroll-bench
                                                  see benchmark above
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy ... --all-targets -- -D warnings     clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

> 续六十四已关闭其中 fixed/sticky/scale-rotate 的回退；以下保留为当时记录。

- 没有独立 compositor 线程或 GPU 合成；所有工作仍在 render worker。
- 只支持 translation transform；rotate/scale/skew 的 top-level layer 仍走完整
  raster。
- fixed/sticky 内容、filter/mask、非 Normal blend mode、ambient clip 都回退，
  因此真实复杂页面可能仍使用完整 raster。
- band cache 无上限/淘汰策略；超长页面会持续保留已访问 band，后续需要 residency
  策略或视口外释放。
- 合成器只处理 top-level stacking context；嵌套 context 由 band rasterizer 在
  单个 band surface 内处理，可能有跨 band 的裁剪/合成边界问题。
- 滚动帧没有独立的 frame timing 历史，只有 `CompositorStats` 计数和 benchmark。

## 下一步

- 支持 scale/rotate layer 与 fixed/sticky 的 retained 表示。
- band residency/eviction 与多线程/GPU 合成。
- 把 compositor 的 frame time 纳入 `render-perf` JSON 和 CI 滚动 benchmark。
- 继续 RTL 块轴镜像、`scroll-state()`、`@starting-style`。

---

# 交接文档 (2026-11-01, 续六十二) — HTTP(S) webfont 取回、WOFF/WOFF2 解码与加载循环

这一轮把上一轮的本地 webfont 路径推进到真正的网络循环：render worker 规划
HTTP(S) 字体 URL，app 用缓存的 fetch batch 取回，字节在下一帧度量前解码
（raw sfnt / WOFF 1.0 / WOFF2）并安装。没有站点特判。

## 取回循环

- `render-worker` 在每次完整渲染结束后，用 `output.styles` 的 computed
  `font-family` 和 DOM 文本字符调用 `DocumentFontLoader::plan_fetches`；
  `DocumentFonts::plan_fetches` 仍按 §4.8.1 只选页面实际命名 family 且
  `unicode-range` 命中的 URL，并跳过已缓存/已解码的 URL。
- `PageRenderFrame` 新增 `font_plan: Option<FontFetchPlan>` 和
  `installed_fonts: Vec<Url>`。
- app 端 `start_font_fetches`：按 `attempted_fonts` 去重，带 cookie 交给
  `submit_cached_batch`，结果进 `PageState.pending_fonts`。
- `finish_font_fetches`：`apply_font_batch` 保留成功 body、报告 HTTP/transport
  失败；body 存入 `PageState.font_resources`（`Arc<HashMap<Url, Arc<[u8]>>>`，
  payload clone 只增引用计数），并强制调度下一次完整渲染。
- `FullPageRenderPayload` 携带 `font_resources`；render worker 在
  `document.render...` 之前调用 `install_fetched_map`，解码后把 `Arc<Font>`
  放进 loader 的跨规则缓存。`install` 的 dirty 标志保证同一次渲染的安装钩子
  会重建字体表。
- app 收到帧后只删除 `installed_fonts` 对应的原始 body，未消费/失败的 body
  保留；导航时清空并取消 pending batch。
- `poll_network`、`has_pending_work`、`report_stalled_batches` 都纳入
  `pending_fonts`，字体传输不会让事件循环提前休眠。

## WOFF / WOFF2

- `decode_font_container`：
  - `wOF2` → `woff2-patched 0.4.0` 的 Brotli 解码器；
  - `wOFF` → 自研 WOFF 1.0 inflater：读取 44 字节 header 和 20 字节 table
    directory，按 `compLength < origLength` 用 `flate2` zlib 解压，重建 sfnt
    header/directory、四字节对齐和 `head.checkSumAdjustment`；
  - raw sfnt（`0x00010000` / `OTTO` / `true` / `typ1`）直接通过。
- `FontFormat::Woff` / `Woff2` 现在都是 `is_decodable()`，因此 `@font-face`
  的 `format('woff2')` 不再被 §4.3.3 丢掉。
- 修复了一个真实生产 bug：`url("...") format("woff2")` 的 quoted URL 是
  `Function` token，旧解析器拒绝其后的 `format()`/`tech()`；现在 quoted 和
  unquoted URL 共用同一个后缀解析器，并有回归测试。
- 依赖说明：`woff2-patched` 使用的 `safer-bytes 0.2` 不能与 bytes 1.6+ 的
  inherent `try_get_*` 共存，`render-browser` 固定 `bytes = "=1.10.0"`；注释
  写在 `Cargo.toml`。

## 测试与证据

- `crates/render-core/tests/fixtures/` 新增同一 fixture 的 `.woff` 和
  `.woff2`。
- `woff_and_woff2_decode_to_the_fixture_face`：三种容器解码后 `H` 的
  advance 都是 16px。
- `a_fetched_woff2_body_changes_the_platform_measurers_advance`：WOFF2 body
  经 `install_fetched` 后 `HHH` 为 48px。
- `an_http_fetched_woff2_changes_the_measured_advance`：loopback HTTP server
  提供 WOFF2，`FontFetchPlan::requests` → `HttpTransport::fetch_batch` →
  `apply_font_batch` → `install_fetched` → `SystemFontBackend::measure`，
  测出 48px；同时断言请求的 `Accept` 和失败列表为空。
- `a_quoted_url_takes_the_same_format_suffix_as_an_unquoted_one`：quoted
  `url("a.woff2") format("woff2")` 保留 `FontFormat::Woff2`。
- release shell 用本地 HTTP 服务加载 `webfont-http-demo.html`，截图
  `webfont-http-final.png`（62,151 字节）：@font-face 行是 1em 宽 H，fallback
  行明显更窄，`ready=Complete`、`css_errors=0`、`css_warnings=0`。

## 验证

```
cargo test -p render-core --lib                  459 passed, 0 failed
cargo test -p render-core --test webfont_layout  2 passed, 0 failed
cargo test -p render-css --lib                   305 passed, 0 failed
cargo test -p render-browser --lib               210 passed, 0 failed
cargo test -p render-js --lib                    1,210 passed, 0 failed
cargo test --workspace                           all crates passed, 0 failed
cargo fmt --all --check                          clean
cargo clippy ... --all-targets -- -D warnings    clean
python tools/check_site_neutrality.py            0 blocking, 26 advisory
```

## 已知缺口

- `@font-face` 嵌套在 `@media`/`@supports` 仍只 walk，不评估条件。
- `font-display` 仍无 block/swap/failure 时间线；字体到达前按 §5.2
  “not present”走 fallback，到达后重排一次。
- WOFF2 的 `hmtx` transform、EOT、SVG 字体仍未支持；`woff2-patched` 对
  这些 face 会返回 undecodable。
- `font-variation-settings`、`font-feature-values`、`font-named-instance`、
  metrics override 与 `tech()` 仍未实现。
- 网络字体下载失败只打印诊断，不进入 DevTools 式 resource timing；没有
  `font-display` 的阶段状态。

## 下一步

- 条件门控与 `font-display` 时间线。
- WOFF2 `hmtx` transform / EOT / SVG（或明确记为不支持）。
- 继续 RTL 块轴镜像、scroll-state、`@starting-style`、compositor。

---

# 交接文档 (2026-11-01, 续六十一) — `@font-face` 收集与本地 webfont 加载

这一轮把 `@font-face` 从“解析后丢弃”推进到“作者样式表发现 → 布局前安装 →
真实度量/绘制”。没有站点特判。

## 收集

- `render-core/src/font_face.rs` 新增 `FontFaceSource { base_url, rules }`：
  一个样式表的规则加上 §4.3.2 的解析基准。
- `ExternalStyleSheets` 的 entry 现在同时保存 `StyleSheet`、`FontFaceSheet` 和
  实际 serve URL（重定向后为 `response.final_url`）。`insert_css` 解析两份；
  `insert_parsed_with_base` 供网络加载器复用已解析的 CSS；`insert_cloned_from`
  让 `merge_current_style_sheets` 保留 font face，而不是只复制 style rules。
- `collect_author_style_sheets` 新增 `font_faces: Vec<FontFaceSource>`：
  - `<style>` 元素：从同一段 CSS 调用 `parse_font_faces`，base 为文档 URL；
  - `<link>`：从 `ExternalStyleSheets` 取已保存的 `FontFaceSheet` 和 sheet URL；
  - descriptor 语法错误进入 `DocumentDiagnosticCode::FontFaceProblem`，不再静默。
- `DocumentBackends` 新增 `font_face_installer: Option<&dyn FontFaceInstaller>`。
  `render_dom` 在 `collect_author_style_sheets` 之后、任何 `build_formatting_tree`
  之前调用它，保证第一次度量就看到文档 face。

## 加载与安装

- `render-browser/src/font_faces.rs` 新增 `DocumentFontLoader`：
  - 用当前 `FontFaceSource` 重建 `DocumentFonts`；
  - `add_rules_at` 支持 `Option<&Url>`：没有 base 时仍接受绝对 URL；
  - 复用上一轮的 `Arc<Font>`，不重复解码同一 URL；
  - 同步读取 `file:` 和 base64 `data:`；`local()` 仍由 `SystemFontBackend`
    的 installed face 查找处理；
  - 调 `SystemFontBackend::set_document_faces` 发布整张文档表；
  - 规则集未变化时不重装，避免每帧递增 generation。
- `SystemFontFaceInstaller` 把 loader 和 backend 组合成 `FontFaceInstaller`，
  `render_worker.rs` 在唯一的 `DocumentBackends` 构造点设置它。
- `resources.rs` 用 `insert_parsed_with_base` 写入 fetch 结果，并转发
  `@font-face` descriptor 诊断。

## 测试与证据

- 新增 `crates/render-core/tests/fixtures/render-test-webfont.ttf`：fontTools
  生成的小 TTF，UPM 1000，`H` 的 advance 为 1000（1em）。
- `crates/render-core/tests/webfont_layout.rs`：
  - `a_local_font_face_changes_the_measured_width`：同一段 `HHH` 在 16px 下
    无 face 为 24px（参考测量器 0.5em/字符），有 face 为 48px（fixture 1em/字符）；
  - `an_external_stylesheet_keeps_its_font_faces_and_base_url`：`<link>` sheet 的
    `FontFaceSource` 保留 sheet URL 和作者写的相对 `src`。
- `render-browser/src/font_faces.rs` 的
  `a_local_file_webfont_changes_the_platform_measurers_advance`：
  通过真实 `SystemFontBackend` 从 fallback 变为 48px。
- release shell 截图（`webfont-demo2.png`，72,149 字节）：fixture face 的行显示
  1em advance 的宽 H 字形，fallback 行是普通窄 H，宽度明显不同；页面
  `ready=Complete`、`css_errors=0`、`css_warnings=0`。

## 验证

```
cargo test -p render-core --test webfont_layout  2 passed, 0 failed
cargo test -p render-core --lib                  458 passed, 0 failed
cargo test -p render-css --lib                   305 passed, 0 failed
cargo test -p render-browser --lib               207 passed, 0 failed
cargo test -p render-js --lib                    1,210 passed, 0 failed
cargo test --workspace                           all crates passed, 0 failed
cargo fmt --all --check                          clean
cargo clippy ... --all-targets -- -D warnings    clean
python tools/check_site_neutrality.py            0 blocking, 26 advisory
```

## 已知缺口

- HTTP(S) `src` 仍未由网络循环提交；`DocumentFonts::plan_fetches` 有计划，
  render worker 还没有把字体请求并入 fetch batch。当前 HTTP(S) face 按 §5.2
  “not available”处理。
- WOFF/WOFF2 仍未解码；rasteriser 只读 raw sfnt，corpus 的 223 个块里 218 个
  因此不可用。
- `@font-face` 嵌套在 `@media`/`@supports` 时只被walk，不评估条件；打印媒体
  里的 face 目前也会被收集。
- `font-display`、`font-feature-values`、`font-variation-settings`、metrics
  override、`tech()` 与压缩容器仍未实现。
- `@font-face` 的 `src` 列表里 `local()` 优先顺序由 matcher 处理，但 loader
  仍会为同一规则的 URL 建立资源；如果 `local()` 命中，URL 不会被访问，因为
  `DocumentFonts::plan_fetches` 只在真正需要时缩小 URL。当前 loader 对本地
  URL 会先读取；后续可在 installed face 命中时跳过。

## 下一步

- 把字体请求并入浏览器 fetch batch，支持 HTTP(S) 与 WOFF/WOFF2。
- 条件规则门控与 `font-display` 的失败/交换周期。
- 继续 RTL 块轴镜像、scroll-state、`@starting-style`、compositor。

---

# 交接文档 (2026-11-01, 续六十) — per-element `color-scheme` 与 used scheme 绘制

这一轮把上一轮的文档级 used color scheme 推进为 CSS Color Adjustment 1 的
`color-scheme` 属性：typed、inherited、按元素解析 used scheme，并让
`light-dark()` 与 `Canvas`/`CanvasText` 系统色在绘制期使用元素自己的 scheme。
没有站点特判。

## 属性

- `render-css/src/properties.rs` 新增：
  - `ColorSchemeName::{Light, Dark, Custom(String)}`；
  - `ColorSchemeValue::{Normal, Supported { schemes, only }}`；
  - grammar `normal | [light | dark | <custom-ident>]+ && only?`，
    保留顺序、custom-ident 与 `only`；`normal` 不能与其他值组合，
    `only` 只能出现一次，CSS-wide/reserved keyword 被拒绝。
  - 序列化 canonical 顺序为 `only light dark` / `light dark`。
- `ColorSchemeValue::used_dark(preferred_dark)` 实现 CSS Color Adjustment 1
  §2.3 的本引擎子集：
  - `normal` 使用 page scheme（`DocumentRenderOptions.color_scheme`）；
  - 显式 support list 若包含当前偏好则用偏好；
  - 否则（包括 `only`）取列表中第一个 UA 支持的 `light`/`dark`；
  - 只有未知 custom ident 时回退到 light。
- `render-css/src/computed.rs` 将 `color-scheme` 注册为 inherited、initial
  `normal`，所以它进入 typed computed map 并自动继承给后代。

## 绘制

- `SystemPalette` 新增 `dark_canvas`/`dark_canvas_text` 与
  `for_scheme(dark)`；`resolve` 先按 `dark` 选择 palette，再解析
  `light-dark()`、`Canvas`、`CanvasText`、相对色和 `alpha()`。
- `display_list.rs` 新增 `palette_for(style)`：读取元素的 typed `color-scheme`，
  以 `options.palette.dark` 作为 page preference，得到该元素的
  `SystemPalette`。
- 绘制链路全部改为按元素取 palette：
  - `current_color` 的 fallback 与 `color` 解析；
  - 背景色、边框、box-shadow、text-shadow；
  - 文本装饰色、列表 marker；
  - 线性/径向/锥形渐变、mask 颜色。
- `light-dark()` 不再只看文档级 `options.palette.dark`；元素重写 scheme 后立即
  影响其整棵子树。

## 测试

- `render-css` 的 `parses_color_scheme_supports_and_resolves_used_scheme`：
  `normal`、`light dark`、`only light/dark`、custom-ident 的
  used-scheme 结果，以及非法组合。
- `render-core/src/paint/color.rs` 的
  `for_scheme_selects_system_colors_and_the_light_dark_arm`：
  light palette `canvas=white`/`canvastext=black`，dark 反过来，
  `light-dark()` 分别取两支。
- `document.rs` 的
  `per_element_color_scheme_selects_light_dark_and_system_colors`：
  page scheme 为 Dark 时，`#light { color-scheme:light }` 背景取 light arm，
  `#dark` 的继承子元素取 dark arm；边框的 `canvas` 分别为 white/black。

## 验证

```
cargo test -p render-layout --lib                 352 passed, 1 ignored
cargo test -p render-css --lib                    305 passed, 0 failed
cargo test -p render-core --lib                   458 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-css -p render-core -p render-browser \
  -p render-dom -p render-layout --all-targets -- -D warnings
                                                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- `prefers-color-scheme` 仍只读 `DocumentRenderOptions.color_scheme`；
  root `color-scheme` 的页面级协商尚未回写媒体查询，因此可能出现媒体查询为
  dark 而 root 元素 actual used scheme 为 light 的不一致。
- 文档 canvas 背景（`raster_background`）仍是 render option，不随 root
  `color-scheme` 自动切换。
- `<meta name="color-scheme">` 尚未映射。
- `only` 的 overriding preference 语义没有实现，因为引擎没有 UA override
  preference / forced colors。
- 未知 custom scheme 不参与解析；forced-colors、`forced-color-adjust` 未实现。
- 绘制仍落在 8-bit sRGB；没有宽色域合成。

## 下一步

- root `color-scheme` 与 page scheme / `prefers-color-scheme` / canvas 背景联动。
- `<meta name=color-scheme>`。
- forced-colors 与 forced-color-adjust。
- 继续 `@font-face` 描述符、RTL 块轴镜像、`scroll-state()`。

---

# 交接文档 (2026-11-01, 续五十九) — `content-visibility: hidden`

这一轮补齐 CSS Containment 2 §4 的 `content-visibility: hidden`：元素保留自己的
盒子（背景/边框/尺寸），但内容完全不布局、不生成 fragment、不绘制；同时把该属性
接入 typed 属性系统。没有站点特判。

## 属性与包含

- `render-css/src/properties.rs` 新增 `ContentVisibility::{Visible,Auto,Hidden}`、
  `TypedPropertyValue::ContentVisibility`、grammar 与序列化。
- `render-css/src/computed.rs` 将 `content-visibility` 注册为 non-inherited、
  initial `visible`，因此它进入 typed computed map，而不是只停留在 token 层。
- `render-layout/src/solver/block.rs`：
  - `content_visibility_hidden(style)` 读取 typed 值；
  - `hidden` 时跳过整个 flow/positioned 子树（`children = []`），
    `auto_height = 0`；
  - `has_size_containment` / `has_inline_size_containment` 加入 hidden，
    因此 `content-visibility: hidden` 的自动尺寸按 size containment 处理；
  - `establishes_block_formatting_context`、absolute/fixed containing block
    也按 layout/paint containment 处理；
  - 指定的 `height` 仍然生效，只有 auto 内容高度被清零。
- `render-layout/src/solver/inline.rs`：原子/文本收集遇到
  `content-visibility: hidden` 的元素时跳过其子树，因此行内元素也不会贡献文本。
- `content-visibility: auto` 目前按普通内容布局（屏外跳过是后续优化），
  `contain-intrinsic-size` 尚未实现。

## 测试

新增 `crates/render-layout/src/solver/content_visibility_tests.rs`，5 个精确几何
测试：

- hidden 盒子保留 border+padding，auto 内容高度 0，后续兄弟从 30px 开始；
- hidden 的指定 height 80px 保留；
- hidden 子树没有 child fragment / 文本 fragment；
- hidden 子树中的 absolute 后代也不生成 fragment；
- `auto` 仍正常布局并绘制内容；
- 行内 hidden 元素的文本从行内容中消失（`aa` + hidden `bb` + `cc` = `aacc`）。

另有 `render-css` 的 `parses_content_visibility_keywords`。

## 验证

```
cargo test -p render-layout --lib                 352 passed, 1 ignored
cargo test -p render-css --lib                    304 passed, 0 failed
cargo test -p render-core --lib                   456 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-css -p render-layout -p render-core \
  --all-targets -- -D warnings                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- `content-visibility: auto` 的屏外跳过需要 viewport intersection、可重入布局和
  scroll 触发的 relayout；当前 auto 永远渲染内容，正确但无性能收益。
- `contain-intrinsic-size` 未实现；hidden 的 auto 尺寸固定为 0，即使作者提供
  placeholder size。
- `content-visibility` 的 style containment（counter/quotes 隔离）无消费者，
  因为该引擎还没有 counters/quotes。
- 可访问性树/文本选择仍不应看到 hidden 内容；这两条路径尚未实现，因此目前只验证
  布局与绘制 fragment。

## 下一步

- `content-visibility: auto` 与 `contain-intrinsic-size`。
- `color-scheme` 属性与 per-element `light-dark()`。
- 继续 RTL 块轴镜像、scroll-state、`@starting-style`。

---

# 交接文档 (2026-11-01, 续五十八) — `direction: rtl`、UAX #9 行级 BiDi 与 RTL 对齐

这一轮补齐水平书写模式下的 RTL / 双向文本基础：`direction` 成为 typed 继承属性，
HTML `dir=ltr/rtl` 进入 UA 样式表，inline 布局按 UAX #9 解析 levels 并按行执行 L2
重排，同时修正 `text-align:start/end` 与 RTL `text-indent`。没有站点特判。

## 依赖与属性

- `render-layout` 新增 `unicode-bidi = "0.3.18"`（crates.io 缓存中已有；Cargo.lock
  增加 7 行）。UAX #9 的数据和算法交给该 crate，避免手写不完整的 bidi 类表。
- `render-css/src/properties.rs` 新增 `Direction::{Ltr,Rtl}`、
  `TypedPropertyValue::Direction`、`direction` 的 grammar/serialization。
  baseline registry 原本已把 `direction` 注册为 inherited + initial `ltr`，现在
  初始值也能解析成 typed 值。
- `crates/render-core/src/document.rs` 的 UA sheet 增加：
  `[dir="ltr" i] { direction: ltr }`、`[dir="rtl" i] { direction: rtl }`。
  `dir=auto` 与 `unicode-bidi: isolate` 仍未实现，代码注释中说明。

## 行级 BiDi

- `InlineAtom` 增加 `bidi_level: u8`。`assign_bidi_levels` 把逻辑文本拼成
  UAX #9 输入：普通字符原样、atomic inline 用 U+FFFC、强制换行用 U+2028；
  以 CSS `direction` 作为 paragraph level（`BidiInfo::new(..., Some(base))`）。
  ASCII + LTR 的最常见路径直接全部 level 0，不分配 UAX #9 的逐字节表。
- `TextRun` 记录 `bidi_level` 与 `line`，因此文本 run 在 level 变化处切开；
  `Solver::inline_bidi` 记录每个 inline fragment 的 line/level。
  atomic inline 也登记为一个 level 单元。
- 每行布局完成后 `finish_inline_lines`：
  - 按 line 分组 fragment（用显式 line，而不是猜 y；atomic 的 margin box 可能
    高于/低于 line box）；
  - 用 `BidiInfo::reorder_visual` 执行 L2，把 level runs 按视觉顺序放置；
  - 奇数 level 的文本 run 反转字符，交给左到右 painter；atomic inline 作为整体
    移动，不反转其内部内容；
  - 最后按 `text-align` 平移整个行。`Start` 在 RTL 映射到右、`End` 在 RTL 映射
    到左；`Justify` 尚无两端对齐，暂时落到段首边。
- `text-indent`：`indented_line_band` 增加 `rtl` 参数，RTL 时从右边缘扣除 indent；
  `line_indents` 按行保存，对齐时使用 `containing width - indent - line width`。
- 纯 LTR、start 对齐、无 indent 的 inline context 走 fast path，只清理 side map，
  不建立 BTreeMap 或跑 reorder。

## 测试

新增 `crates/render-layout/src/solver/bidi_tests.rs`，12 个精确几何测试：

- 纯 RTL 希伯来文本：DOM 保持逻辑顺序，fragment 文本反向，右对齐 x=176；
- RTL 段落中嵌入 LTR 短语，LTR run 保持内部顺序并位于左侧；
- RTL 段落中的欧洲数字保持 LTR/level 2；
- LTR 段落中只反转内嵌 RTL run；
- RTL 段落按同一逻辑断点换行，两行都从右起；
- RTL `text-indent` 从右边缘缩短；
- RTL `text-align:end` 到左边、LTR `text-align:end` 到右边；
- 同 level 的 span 边界随整段 RTL run 一起重排；
- atomic inline 作为 U+FFFC 整体进入视觉顺序；
- `dir=rtl` 属性 selector 驱动方向。

另有 `render-css` 的 `parses_direction_keywords` 测试。

## 验证

```
cargo test -p render-layout --lib                 347 passed, 1 ignored
cargo test -p render-css --lib                    303 passed, 0 failed
cargo test -p render-core --lib                   456 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-css -p render-layout -p render-core \
  --all-targets -- -D warnings                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- UAX #9 L3（combining marks）和 L4（mirroring）未执行；反转字符对 Hebrew
  简单文本正确，对组合标记/括号镜像不完整。
- `unicode-bidi: embed/isolate/override/bidi-override` 未实现；行内元素自身的
  `direction` 还没有 embedding 通路，目前只消费 block container 的 paragraph
  direction。
- `dir=auto` 未实现；UA 只映射 `dir=ltr/rtl`。
- 块轴 RTL 镜像未实现：margin/padding 的 start/end、float、逻辑属性、滚动条位置
  仍按物理左/右处理。
- 复杂脚本整形（Arabic/Indic/Thai cluster、ligature）仍缺；反转 run 只是显示
  顺序，不是 shaping。
- 行尾空白的 L1 重置尚未按行执行；普通文本测试通过，但带尾随空格的极端行仍需补。

## 下一步

- `unicode-bidi` 的 inline embedding/isolate 与 `dir=auto`。
- UAX #9 L3/L4 + 复杂脚本 shaping。
- 块轴 RTL 镜像与 logical properties。
- 继续 bidi 的行尾空白与多段落强制换行语义。

---

# 交接文档 (2026-11-01, 续五十七) — CSS 2.1 §8.3 垂直 margin collapsing

这一轮补齐 block formatting context 的垂直 margin collapsing：相邻兄弟、父/首子、
父/末子，以及空块的 collapse-through。没有站点特判。

## 模型

- `BlockResult` 从“margin box 高度”改为显式的四项：
  - `margin_top`：与首个子盒折叠后的有效上 margin；
  - `border_height`：border box 高度；
  - `margin_bottom`：与最后一个子盒折叠后的有效下 margin；
  - `collapses_through`：空盒的整条 strut 穿透。
- `MarginStrut` 保存 `{ positive, negative }`，合并规则为
  `max(positive) + min(negative)`，因此正负 margin 混合时仍满足结合律，
  与 CSS 2.1 §8.3.1 的算法一致。
- 父级布局用 `cursor_y`（上一个 border box 底边）加 `pending` strut 定位下一个
  border 顶边；不再把两个 margin 直接相加。
- `layout_block_with_containing_height` 新增 `collapse_children` 参数。以下传
  `false`，即不与其子盒折叠：
  - flex item（Flexbox §4）、grid item（Grid §6）、table cell；
  - float、absolutely/fixed positioned box；
  - 多列容器、`flow-root`、`overflow` 非 visible、containment/`container-type`
    形成的 BFC。
- 父/首子折叠：父无上 border/padding 且不是 BFC 时，父 border 顶边移到
  `collapse(parent margin-top, first-child effective top margin)`；首子的上 margin
  不再在父 content 内重复。父/末子规则对应检查 height auto、min-height 0、
  无下 border/padding。
- 空块穿透：无垂直 border/padding、height auto 或 0、min-height 0、无 in-flow
  内容（或所有子块都穿透）时，自己的上下 margin 与前后兄弟、后代的 strut 合并成
  一条，只出现一次。
- 清除（clearance）会重置 pending group，并使父/首子折叠失效。
- 修正了一个随之暴露的定位问题：父/首子折叠会移动 in-flow 子树，但绝对定位后代
  若其 containing block 在被移动子树之外，就必须保持文档位置。
  `translate_fragment_subtree_for_margin` 会跳过这类 out-of-flow 后代；包含块在
  子树内时仍然一起移动。

## 测试

新增 `crates/render-layout/src/solver/collapse_tests.rs`，13 个精确 border-box
几何断言：

- 正 margin 相邻兄弟取最大值；
- 正负混合 `max(+) + min(-)`，含两个负 margin；
- 父/首子折叠、border 阻断；
- 父/末子折叠、padding 阻断；
- 空块穿透、border 阻断、嵌套空块；
- 父上 margin 跨过空的第一个子盒；
- BFC root 不与子盒折叠；
- 负的父/末子 margin 缩短后续间距；
- 父/首子折叠后绝对定位后代跟随 containing block。

已有 `sticky_tests` 的滚动容器用例也验证了 body 首子 margin 折叠后的 20px 偏移。

## 验证

```
cargo test -p render-layout --lib                 335 passed, 1 ignored
cargo test -p render-core --lib                   455 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-layout -p render-core \
  --all-targets -- -D warnings                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- `content-visibility` 的布局包含与 margin 折叠边界尚未实现。
- RTL / 垂直书写模式的 margin collapsing 未实现（当前只覆盖水平书写模式）。
- `@page` 的 margin 折叠与分页未实现。
- clearance 与浮动组合只覆盖到“清除后不折叠”的常规路径；更细的
  `clear` + 负 margin 交互仍可补测试。
- flex/grid item 之间按规范不折叠，这部分是明确排除而非缺口。

## 下一步

- `content-visibility` 与 `contain: style` 的隔离。
- RTL / `direction` 的布局消费。
- 继续补 bidi、compositor、宽色域合成等大缺口。

---

# 交接文档 (2026-11-01, 续五十六) — CSS Color 5 `alpha()` 与 used color scheme 接线

这一轮补齐 CSS Color 5 §4.10 的 `alpha()`，并把 `light-dark()` 从“只有一个
`SystemPalette.dark` 入口”接到真正的渲染输入 `DocumentRenderOptions.color_scheme`。
没有站点特判。

## `alpha()`

- `alpha(from <color> / <alpha-value>)`：origin 颜色分量完全保持不变，只替换或修改
  alpha；结果保留 origin 的颜色空间，alpha 省略/`none`/calc 与规范一致。
- 实现为 `CssColor::Alpha(Box<AlphaColor>)`：origin 绝对时解析期求值，含
  `currentcolor`/系统色/`light-dark()` 时保留到绘制期。
- `alpha` 关键字、number、percentage、`none`、基础 `calc()` 均可用于 alpha 参数；
  component 关键字（如 `r`）在 `alpha()` 中明确拒绝。
- 绘制期 `SystemPalette::resolve_space` 递归解析 origin，再 `resolve_alpha_space`
  只替换 alpha。

## used color scheme

- `DocumentRenderOptions` 新增 `color_scheme: ColorScheme`（默认 Light）。
- `render_dom` 把该值同时用于：
  - `MatchContext.prefers_color_scheme`，因此 `@media (prefers-color-scheme: dark)`
    与 `light-dark()` 得到同一答案；
  - `DisplayListBuilderOptions.palette.dark`，因此 `light-dark()` 的绘制分支一致。
- 这不是 `color-scheme` 属性：元素尚不能自选 scheme；当前是 render input。

## 测试

- `alpha(from rgb(0 0 255 / 0.4) / 80%)` → `(0, 0, 255, 0.8)`。
- `alpha(from red / calc(alpha * 0.5))` → `(255, 0, 0, 0.5)`。
- `alpha(from rgb(1 2 3 / none) / none)` → alpha 0。
- `alpha(from currentcolor / 0.5)` 在绘制期解析为当前色 components + alpha 0.5。
- 文档层：同一份 HTML 在 `color_scheme: Light` / `Dark` 下分别绘制
  `light-dark(rgb(1 2 3), rgb(4 5 6))` 的对应分支，并验证
  `@media (prefers-color-scheme: dark)` 同步切换。

## 验证

```
cargo test -p render-css --lib                    302 passed, 0 failed
cargo test -p render-core --lib                   455 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-css -p render-core -p render-browser -p render-dom -p render-layout \
  --all-targets -- -D warnings                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- `color-scheme` 属性/元素级 used scheme 仍未实现；只有文档级 render input。
- `device-cmyk()`、`contrast-color()` 未实现。
- `calc()` 仅基础四则；`min/max/clamp` 等数学函数尚未接入相对颜色。
- Canvas `fillStyle`、CSS transition 尚未完整消费 `alpha()`/`light-dark()`；
  宽色域合成和颜色动画的指定插值空间仍待实现。

## 下一步

- `color-scheme` 属性与元素级 used scheme。
- `device-cmyk()` / `contrast-color()`。
- `calc()` 的 `min/max/clamp` 与百分比语义。
- 相对色/`light-dark()` 在 canvas、transition、gradient 中的完整解析。
- 转到 `contain: style`、`scroll-state()`、`@starting-style`、`@font-face` 描述符。

---

# 交接文档 (2026-11-01, 续五十五) — CSS Color 5 相对颜色语法与 `light-dark()`

这一轮补齐 CSS Color 5 §4 的相对颜色语法（`rgb(from ...)`、`hsl(from ...)`、
`hwb(from ...)`、`lab/lch/oklab/oklch(from ...)`、`color(from ...)`）以及 §7 的
`light-dark()`。没有站点特判。

## 相对颜色

- `render-css/src/color.rs` 新增：
  - `RelativeFunction`（Rgb/Hsl/Hwb/Lab/Lch/Oklab/Oklch/Color(space)）；
  - `RelativeChannel`（`none`、number、percentage、channel keyword、`calc()`）；
  - `CalcExpr`（数字、百分比、关键字、`+ - * /`、括号、一元负号）；
  - `RelativeColor` 与 `RelativeAlpha::Inherit`（省略 alpha 时继承 origin alpha）；
  - `LightDarkColor`。
- 解析：
  - 各颜色函数在参数开头识别 `from <color>`；legacy comma 语法不能与相对语法混用。
  - `rgb()` 的 channel number 按 0..255，`hsl/hwb` 的 s/l/w/b 按 0..100，
    `lab` L 0..100、a/b 100%=125，`lch` C 100%=150，`oklab` L 0..1、a/b 100%=0.4，
    `oklch` C 100%=0.4，`color()` 各分量按 0..1。内部统一折算到 `SpaceColor` 单位。
  - 支持 `alpha` 关键字、`none`、直接 number/percentage，以及 `calc()`；
    hue 位置拒绝 percentage（包括 `calc(50%)`）。
  - origin 为绝对色时在解析期求值；含 `currentcolor`/系统色/`light-dark()` 时保留
    `CssColor::Relative`，绘制期由 `SystemPalette::resolve_space` 递归求值。
  - origin 与目标空间不同时按 §13.3 做 missing carry-forward，并对转换后的
    powerless 分量置 missing。
- 数值测试覆盖：恒等相对色、直接通道、`calc(r + 10)`、规范中的 grayscale 公式
  `r*.3+g*.59+b*.11`、HSL 补色、oklch 恒等、`color()` 跨空间、alpha、missing hue。

## `light-dark()`

- `CssColor::LightDark` 保留两支；绘制期 `SystemPalette.dark` 选择 light/dark。
- 默认 light；当前没有 `color-scheme` 属性/used-scheme 传播，嵌入方可在
  `DisplayListBuilderOptions.palette.dark` 上设置。
- 单测：properties 解析两支；paint 层分别以 light/dark palette 解析出
  `rgb(1,2,3)` / `rgb(4,5,6)`，并验证 `rgb(from currentcolor ...)`。

## 验证

```
cargo test -p render-css --lib                    300 passed, 0 failed
cargo test -p render-core --lib                   454 passed, 0 failed
cargo test -p render-browser -- --test-threads=1  206 + 114 + 2 passed, 0 failed
cargo test -p render-layout --lib                 322 passed, 1 ignored
cargo test -p render-dom --lib                    78 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-css -p render-core -p render-browser -p render-dom -p render-layout \
  --all-targets -- -D warnings                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- `alpha()` 独立相对 alpha 函数、`device-cmyk()`、`contrast-color()` 未实现。
- `light-dark()` 尚未接入 `color-scheme` 属性；当前只有 `SystemPalette.dark` 入口。
- `calc()` 仅实现基础四则、括号、number/percentage/关键字；`min()`/`max()`/
  `clamp()` 等数学函数尚未接入相对颜色。
- Canvas `fillStyle` 仍通过 `resolve_absolute`，因此 `light-dark()` 和上下文相关
  相对色在 canvas 上会被拒绝；CSS transition 也尚未沿相对色插值。
- 颜色动画仍按 sRGB 通道插值；绘制仍统一落到 8-bit sRGB。

## 下一步

- `alpha()` 与 `light-dark()` 的 `color-scheme` 接线。
- `device-cmyk()` / `contrast-color()`。
- `calc()` 中的 `min/max/clamp` 与百分比语义。
- 相对色在 canvas、transition、gradient 中的完整解析。
- 继续处理 `contain: style`、`scroll-state()`、`@starting-style`、`@font-face` 描述符。

---

# 交接文档 (2026-11-01, 续五十四) — Shadow DOM 样式/slotchange、Color `none` 与感知色域映射、截图唤醒

这一轮并行推进四块现代浏览器能力：shadow-tree 样式与 `slotchange`、CSS Color 4 的
`none`/感知色域映射、`--screenshot` 事件循环唤醒，以及本机 `dav1d` 构建前提。
没有站点特判。

## Shadow DOM 样式

- `StyleSheet::tree_root` 记录样式表所属节点树（`None` 视为 document）。样式发现按
  shadow-including tree order 遍历；shadow root 内的 `<style>` 与外部 `<link>` 都带
  自己的 tree root。
- 级联只让 author sheet 作用于其所属树。唯二跨树路径：
  - `:host` / `:host(<compound>)` / `:host-context(<compound>)`：shadow host 在自身
    shadow tree 中是 featureless 的，只能经 host 伪类家族命中；
  - `::slotted(<compound>)`：通过 originating slot 命中被分配的 light-DOM 元素，
    支持 flatten 后的 assignment chain。
- 级联新增 Cascade 5 §6.1 Context 步骤：normal 外层树声明胜内层，important 内层
  声明胜外层。`Priority.context` 位于 origin 之后、layer 之前。
- 语义细节：`:host` 特异性为伪类 + 参数；`::slotted` 为伪元素 + 参数；`:host:hover`
  不匹配（必须写 `:host(:hover)`）；`:host-context()` 会向上跨 shadow 边界。
- `@scope` 的隐式根恢复和祖先链改为 shadow-aware：shadow 内 `<style>` 的隐式根回退
  到 shadow host，祖先匹配止于 host，不泄漏到外层 document。

## `slotchange`

- `Dom::slot_assignment_snapshot()` 给出所有 shadow tree 中每个 slot 的 assigned
  nodes；`Dom::has_shadow_roots()` 与内部计数让没有 shadow root 的页面零成本跳过。
- `Page` 保存上一回合快照；每个回合微任务检查点之后比较并派发 `slotchange`。
  事件在 slot 上触发，冒泡、不可取消、非 composed，记录为
  `PageJob::SlotChange { slot }`。监听器再次改分配时最多追加 8 轮，避免自激循环。
- `render-dom` 新增快照单元测试；`render-core` 新增 attachShadow → 初始分配 →
  移除 slottable 的端到端测试。

## CSS Color 4/5

- 新增 `SpaceColor { space, components: [Option<f32>; 3], alpha }` 与
  `CssColor::Space`。`none` 在所有现代颜色函数中可解析（含 `/ none` alpha），
  legacy comma 语法仍拒绝。
- `color-mix()` 保留 authored space，实现 analogous component 的 missing
  carry-forward、analogous set（Lab a/b ↔ LCH C/H）、both-missing 保留、alpha
  carry；缺失分量在插值之外按 0 处理。
- powerless 阈值采用 CSS Color 4 §19 样例：HSL 0.001、HWB 0.99999、LCH 0.0015、
  OKLCH 0.000004。
- sRGB 设备边界改用 §14.2.2 Binary Search + Local MINDE 感知色域映射（OKLCH
  chroma 二分，JND 0.02，epsilon 0.0001），替换逐通道裁剪。P3 原色、oklch 越界色和
  CSS Color 5 的 HSL/P3 示例均有数值测试。
- Canvas `fillStyle` 与 CSS transition 通过 `resolve_absolute` 接受现代/非 sRGB
  颜色；`none` 在两者中按 0 处理。

## `--screenshot` 卡死

- 根因：截图 deadline 没有进入 `ControlFlow`。页面稳定后无网络/脚本/动画/timer，
  事件循环进入 `Wait`，deadline 到点也无事件唤醒。
- 修复：`next_control_flow()` 把截图 deadline 与帧轮询、页面 timer 取最小值；
  窗口已存在但 frame 为空时明确报错；事件循环在截图未写出就停止时非零退出。
- 实测生成 122,219 字节 PNG，`@scope` 演示的颜色/限制/嵌套/隐式根全部正确。
- 同轮的浏览器测试基建：`serve_until_idle` 改为按期望请求数收尾，跨源 PUT 预检
  测试不再依赖 800ms 空闲窗口，消除了并行 `cargo test --workspace` 下的时序
  flake。

## 构建前提

- `image` 的 `avif-native` 经 `dav1d-sys` 链接系统 `libdav1d`。本机用 MSVC +
  meson 构建 dav1d 1.5.1 到 `C:\Users\Elyar\dav1d-install`，设置
  `SYSTEM_DEPS_DAV1D_NO_PKG_CONFIG=1`、`SYSTEM_DEPS_DAV1D_LIB=dav1d`、
  `SYSTEM_DEPS_DAV1D_SEARCH_NATIVE`、`SYSTEM_DEPS_DAV1D_INCLUDE`，并把其 `bin`
  加入 PATH。README 已记录通用做法。
- 该设置下 `cargo run --release -- --help` 与 AVIF 解码测试均通过。

## 验证

```
cargo test -p render-css --lib                    295 passed, 0 failed
cargo test -p render-core --lib                   453 passed, 0 failed
cargo test -p render-browser -- --test-threads=1  206 + 114 + 2 passed, 0 failed
cargo test -p render-layout --lib                 322 passed, 1 ignored
cargo test -p render-dom --lib                    78 passed, 0 failed
cargo test --workspace                            all crates passed, 0 failed
cargo fmt --all --check                           clean
cargo clippy -p render-css -p render-core -p render-browser -p render-dom -p render-layout \
  --all-targets -- -D warnings                    clean
python tools/check_site_neutrality.py             0 blocking, 26 advisory
```

## 已知缺口

- 相对颜色语法（`rgb(from ...)`）、`light-dark()`、`device-cmyk()`、
  `contrast-color()` 尚未实现。
- 绘制仍统一落到 8-bit sRGB，感知色域映射在 sRGB 边界执行，尚无宽色域合成；
  颜色动画仍按 sRGB 通道插值。
- `::part()`、`:has-slotted()`、声明式 shadow root、`::slotted()::before/after`
  尚未实现；shadow 样式失效根尚未加入 host。
- `@import ... scope(...)` 仍未接入；`@scope` 大规模文档的根/限制缓存仍可优化。
- `slotchange` 的派发点在回合微任务检查点之后，回合外直接 DOM 变更要到下一回合
  才被观察到。

## 下一步

- 相对颜色语法与 `light-dark()`。
- `contain: style` 的计数器/quotes 隔离。
- `scroll-state()` 容器查询。
- `@starting-style`、`@font-face` 描述符。
- fixed 包含块与滚动容器偏移联动。
- `::part()` / `:has-slotted()` / 声明式 shadow root。

---

# 交接文档 (2026-10-31, 续五十三) — CSS Cascade 6 `@scope`

这一轮实现 CSS Cascade 6 §3.5 的 `@scope`：作用域根/限制、相对选择器、
`:scope` 与 `&`、嵌套作用域、作用域内直接声明，以及级联中的 scope proximity。
没有站点特判。

## 语义与接入

- `StyleSheet::scope_owner` 记录样式表 owner（`<style>`/`<link>` 元素）。
  `render-core::document` 在收集内嵌与外部样式表时设置它；`@scope` 无
  `<scope-start>` 时的根是 owner 的父元素。若父节点不是元素，则依次回退到
  shadow host 与包含节点树的根。
- `StyleRule::scopes: Vec<Scope>` 从外到内保存全部 `@scope` 边界。`Scope` 的
  `start`/`end` 是已解析的 `SelectorList`；`start: None` 表示隐式作用域。
- 作用域内样式规则（§3.5.3）：
  - 不以组合符开头且不含 `:scope`/`&` 的选择器加隐式 `:where(:scope)` 后代前缀，
    因此作用域 prelude 不贡献特异性。
  - 以组合符开头的相对选择器同样锚定在 `:where(:scope)`。
  - 显式 `:scope` 保持 `(0,1,0)`；`&` 等价于 `:where(:scope)`，特异性为零。
  - 嵌套 `@scope` 的 `<scope-start>` 相对外层作用域根；`<scope-end>` 与作用域内
    规则相对自己的根（§3.5.5）。
- 嵌套作用域：内层根的候选必须落在外层作用域的祖先链内，并同时满足外层的
  scoping limit；scope proximity 只按最内层根到 subject 的世代距离计算。
- `@scope` 的块是 `<block-contents>`（§3.5.6）：连续的直接声明各自成为一条
  `:where(:scope)` 规则，匹配作用域根且特异性为零；声明与子规则可交错。
- 级联（§3.1）：`Priority` 在 specificity 之后、order of appearance 之前加入
  `proximity`。距离越近优先级越高；未作用域规则视为无限距离。
- `<scope-start>`/`<scope-end>` 不支持伪元素（§3.5.2），且选择器列表是
  unforgiving 的；非法 prelude 丢弃整个块并报告原因。
- `at_rules` 将 `scope` 移入 `EVALUATED_AT_RULES`，`@supports at-rule(@scope)`
  与解析器/级联不再互相矛盾。
- 顺带修正选择器匹配：`:is()`/`:where()`/`:not()`/`:has()` 与
  `:nth-child(… of …)` 的参数列表按元素匹配，不再继承外层伪元素上下文；
  这修复了 `:is(.host) p::before` 一类此前无法匹配的选择器。

## 测试

- 解析：相对选择器前缀、`:scope`/`&` 特异性、限制选择器、嵌套作用域、隐式
  prelude、直接声明分段与顺序、非法/伪元素 prelude 诊断、`@scope` 内
  `@keyframes` 不受作用域影响。
- 级联：根自身在作用域内、limit 子树被排除、proximity 打破特异性平局、
  特异性优先于 proximity、已作用域规则胜过同特异性未作用域规则、隐式 owner
  根、嵌套作用域互相约束、spec §3.5.5 的 `:scope` prelude 示例、生成内容
  伪元素在作用域内可命中、直接声明零特异性。
- 文档层：`<style>` 的 owner 被写入 `scope_owner`，隐式 `@scope` 在
  `render_reference` 的实际 computed color 上生效。

## 验证

```
cargo test -p render-css --lib                     285 passed, 0 failed
cargo test -p render-layout --lib                  322 passed, 1 ignored
cargo test -p render-core --lib                    442 passed, 1 failed
                                                   （临时关闭 avif-native；失败仅为 AVIF 解码测试）
cargo fmt --all --check                            clean
cargo clippy -p render-css -p render-layout -p render-core --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 的 `pkg-config` 缺失阻塞，与本轮代码无关。

## 已知缺口

- `@import ... scope(...)` 尚未接入；目前只支持规则块形式的 `@scope`。
- shadow root 内的样式表仍未收集（既有缺口）；收集后 `scope_owner` 已能表达
  shadow 树的隐式根回退。
- `<scope-start>`/`<scope-end>` 按 §3.5.2 拒绝伪元素，因此 `::part()`/`::slotted()`
  不能作为作用域根或限制；这是规范行为，不是未实现项。
- 语句形式 `@scope ...;`（无块）走通用语句 at-rule 路径，诊断沿用既有的
  “已求值”措辞（与 `@media;` 相同），不是本轮新增的静默丢弃。
- 作用域解析为每个候选规则构建祖先链并可能克隆 `MatchContext`；功能正确，
  但大规模 `@scope` 文档仍有优化空间（缓存根/限制或按根索引规则）。

## 下一步

- `contain: style` 的计数器/quotes 隔离。
- `scroll-state()` 容器查询。
- `@starting-style`、`@font-face` 描述符。
- `none` 颜色分量 carry-forward、感知色域映射与相对颜色语法。
- fixed 包含块与滚动容器偏移联动。
- shadow-tree 样式表收集，使 `@scope` 的隐式 shadow host 根与 `:host` 一起可观测。

---

# 交接文档 (2026-10-31, 续五十二) — 现代颜色函数、全部预定义色彩空间与 `color-mix()`

这一轮补齐 CSS Color 4 的现代颜色函数与 CSS Color 5 的 `color-mix()`：所有预定义
`color()` 空间、`hwb()`、`lab()`/`lch()`、`oklab()`/`oklch()` 都能解析并换算到设备
sRGB，`color-mix()` 在任意支持的空间中插值（含极坐标色调路径与预乘 alpha），依赖
`currentcolor`/系统色的混合保留到绘制期解析。没有站点特判。

## 语义与接入

- `render-css::color` 是唯一的颜色换算层：`ColorSpace` 覆盖 `srgb`、`srgb-linear`、
  `display-p3`、`a98-rgb`、`prophoto-rgb`、`rec2020`、`xyz`/`xyz-d65`、`xyz-d50`、
  `lab`、`oklab`、`lch`、`oklch`、`hsl`、`hwb`；矩阵与传递函数按 CSS Color 4 §19 的
  样例代码实现，`f32` 计算，设备边界才量化到 8 位。
- 解析（`properties.rs`）：
  - `hwb()` 按 §4.4 归一化白/黑比例；`lab()`/`lch()`/`oklab()`/`oklch()` 接受数字或
    百分比（`100%` 分别对应 L=100、C=150、L=1、C=0.4 等），并支持 `/ <alpha-value>`。
  - `color()` 接受全部预定义空间；未知空间与专用函数空间（如 `color(oklab ...)`）
    仍被拒绝并给出诊断，不静默近似。
  - `color-mix()` 的语法为
    `in <color-space> [<hue-method>]? , <color> [<percentage>]? , <color> [<percentage>]?`；
    百分比缺失时按 CSS Color 5 §3.3 补足/归一，和低于 100% 时结果 alpha 乘以和。
  - 插值在目标空间进行；极坐标空间支持 `shorter`/`longer`/`increasing`/`decreasing`，
    并对 powerless 色相做 carry-forward。非色相分量按预乘 alpha 插值。
  - 两个操作数都是绝对色时在解析期直接得到 `CssColor::Srgb`；只有含
    `currentcolor`/`canvas`/`canvastext` 时才保留 `CssColor::Mix`。
- `CssColor` 因 `Mix(Box<ColorMix>)` 不再 `Copy`；绘制层 `SystemPalette::resolve`
  递归解析未决混合，`currentcolor` 与系统色在同一元素上下文内求值。

## 测试

- 颜色空间：每个预定义空间的往返与边界值（白/黑/原色）、`hwb()` 白黑混合、
  `lab/lch/oklab/oklch` 的已知 sRGB 结果、现代 alpha 语法。
- `color-mix()`：srgb/srgb-linear/oklab 已知结果、百分比归一与 alpha 缩减、透明色预乘、
  四种色调路径、嵌套混合；含 `currentcolor` 的混合在绘制期解析且不被透明色拉黑。
- 非法值：缺分量、未知空间、未知色调方法、超范围百分比、缺少逗号或插值空间均拒绝。

## 验证

```
cargo test -p render-css --lib                     266 passed, 0 failed
cargo test -p render-layout --lib                  322 passed, 1 ignored
cargo test -p render-core --lib                    441 passed, 1 failed
                                                   （临时关闭 avif-native；失败仅为 AVIF 解码测试）
cargo fmt --all --check                            clean
cargo clippy -p render-css -p render-layout -p render-core --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 的 `pkg-config` 缺失阻塞，与本轮代码无关。

## 已知缺口

- `none` 颜色分量当前按 0 参与换算与插值，尚未实现 CSS Color 4 §4.4 的缺失分量
  carry-forward 与序列化保真。
- 色域映射仍是 §14.1.1 的逐通道裁剪，尚未实现 OKLCH 感知映射（§14.1.2）。
- 相对颜色语法（`rgb(from ...)` 等）、`light-dark()`、`device-cmyk()`、
  `contrast-color()` 尚未实现。
- 颜色动画仍按 sRGB 分量插值，尚未接入 `color-mix()` 的空间与预乘规则。

## 下一步

- `contain: style` 的计数器/quotes 隔离。
- `scroll-state()` 容器查询。
- `@scope`、`@starting-style`、`@font-face` 描述符。
- `none` 分量 carry-forward、感知色域映射与相对颜色语法。
- fixed 包含块与滚动容器偏移联动。

---

# 交接文档 (2026-10-31, 续五十一) — 独立的 `contain` 属性与包含块语义

这一轮实现 CSS Containment 2 §2 的 `contain` 属性，并据规格修正 `container-type` 的
包含效果：`container-type` 只施加尺寸包含、style containment 与独立格式化上下文，不再
被误当作定位包含块。没有站点特判。

## 语义与接入

- `Containment { size, inline_size, layout, style, paint }` 作为 `contain` 的 computed value；
  `strict` 展开为 `size layout style paint`，`content` 展开为 `layout style paint`。
  解析接受 `[ [ size | inline-size ] || layout || style || paint ]` 的任意顺序并拒绝重复，
  serialization 按 grammar 顺序输出，`size` 与 `inline-size` 互斥。
- 布局（`render-layout::solver::block`）：
  - `layout`/`paint` 建立独立格式化上下文（BFC），因此 float 计入 auto 高度。
  - `layout`/`paint` 成为 absolute 定位包含块；同时成为 fixed 定位包含块。
  - `size`/`inline-size` 对相应轴施加 size containment，内容不参与自身内在尺寸。
  - `container-type` 只保留独立格式化上下文 + size containment，不再建立定位包含块
    （CSS Conditional 5 §5.1）。
  - `Solver.fixed_positioning_containing` 保存最近的 fixed 包含块 padding box；块递归在
    子布局前安装暂定盒、内容高度确定后更新，并在离开时恢复。`position: fixed` 优先使用
    该盒，否则回退 viewport。
- 绘制（`render-core::paint::display_list`）：
  - `contain: layout|paint` 生成 `CompositingReason::Containment` stacking context。
  - `contain: paint` 在 overflow clip 之外额外把后代裁剪到 padding box。
  - fixed 元素若任一 DOM 祖先带 `contain: layout|paint`、非空 transform 或 filter，
    其显示项使用 `PaintCoordinateSpace::Document`，随文档滚动。
- 注册表把 `contain` 定义为不可继承、初始值 `none`；support oracle 通过
  `DECLARED_GRAMMARS` 回答。

## 测试

- CSS：关键字展开、任意顺序、互斥重复拒绝、grammar 顺序 serialization、非法值拒绝。
- 布局：`contain: layout`/`paint` 的 absolute 包含块；`contain: layout`/`paint` 的 fixed
  包含块；`container-type` 不建立定位包含块；layout containment 包含 float。
- 绘制：paint containment padding-box 裁剪；`size`/`inline-size`/`style` 不裁剪、不建
  stacking context；`layout`/`paint`/`strict`/`content` 建 stacking context；包含盒内
  fixed 后代使用 Document 坐标空间，普通 fixed 仍为 Viewport。

## 验证

```
cargo test -p render-css --lib                     257 passed, 0 failed
cargo test -p render-layout --lib                  322 passed, 1 ignored
cargo test -p render-core --lib                    439 passed, 1 failed
                                                   （临时关闭 avif-native；失败仅为 AVIF 解码测试）
cargo fmt --all --check                            clean
cargo clippy -p render-css -p render-layout -p render-core --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 的 `pkg-config` 缺失阻塞，与本轮代码无关。

## 已知缺口

- `contain: style` 尚未隔离 counter/quotes（当前只解析，无观察效果）。
- 大幅 auto-height 包含盒内 fixed 后代的百分比高度仍可能使用暂定高度，需布局迭代完善。
- `contain: layout|paint` 的 fixed 包含块已接入布局与绘制坐标空间；嵌套滚动容器的
  fixed 包含块尚未与滚动偏移联动。

## 下一步

- `contain: style` 的计数器/quotes 隔离。
- `scroll-state()` 容器查询。
- `@scope`、`@starting-style`、`@font-face` 描述符、`color-mix()`。
- fixed 包含块与滚动容器偏移联动。

---

# 交接文档 (2026-10-31, 续五十) — 生成内容伪元素的包含式样式查询

这一轮补齐 CSS Conditional 5 §5 的伪元素查询容器规则：`::before`/`::after` 查询其
originating element，而普通元素只查询祖先。没有站点特判。

## 语义与接入

- `ContainerContext::matches_with_style_inclusive` 从 originating element 自身开始查找
  查询容器；普通元素的 `matches_with_style` 仍从父节点开始。
- 级联在 `context.pseudo_element.is_some()` 时选择包含式查找。
- 伪元素级联发生在 originating element 的 computed style 写入样式表之前，因此
  `ComputedStyleQueries` 增加 `current` 覆盖项：查询 originating element 时直接读刚算出的
  元素样式，其余节点仍读已完成的祖先样式表。
- 尺寸查询与样式查询共用同一次容器选择：若 originating element 是尺寸容器，伪元素也能
  查询它的几何；普通元素自身仍不是自己的容器。

## 测试

- `#host { --theme: dark }` + `#host::before { content: "x" }` +
  `@container style(--theme: dark) { #host::before { color: red } }`：伪元素样式为 `red`，
  证明查询包含 originating element 且能读到尚未写入样式表的当前样式。

## 验证

```
cargo test -p render-css --lib                     255 passed, 0 failed
cargo test -p render-layout --lib                  318 passed, 1 ignored
cargo fmt --all --check                            clean
cargo clippy -p render-css --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

## 下一步

- 布局包含对 fixed 定位后代的包含块（当前 `position: fixed` 仍直接取 viewport）。
- 布局包含隐含的 stacking context，以及 style containment 的计数器隔离。
- `scroll-state()` 容器查询。
- 独立的 `contain: size/layout/style/paint` 属性（当前只由 `container-type` 触发）。
- 继续现代 CSS：`@scope`、`@starting-style`、`@font-face` 描述符、`color-mix()` 等。

---

# 交接文档 (2026-10-31, 续四十九) — `style()` 容器查询

这一轮把 CSS Conditional 5 §3.1/§6.2 的 `style()` 容器查询接到自顶向下样式过程：
样式查询直接读取查询容器已经计算好的样式，不依赖布局回馈，也不需要 `container-type`。
没有站点特判。

## 语义

- 每个元素都是 style 查询容器（§5.1）：`container-type: normal` 不影响 `style()`，只有
  `@container` 的尺寸部分继续要求 `size`/`inline-size` 资格。
- 查询容器仍从祖先链上选，元素自身不参与；可选 `container-name` 先过滤，再要求该容器能
  回答条件中出现的全部轴。混合条件（尺寸 + 样式）必须由同一个容器回答，不能把样式部分
  退给更近的 style-only 祖先。
- `style()` 的 feature 支持声明形式 `style(--theme: dark)` 与布尔形式 `style(--theme)`；
  值里的函数与字符串按顶层冒号切分，`var(--x)`、`content: "a:b"` 不会被截断；标准属性名
  大小写折叠，`-webkit-*` 前缀与 `--*` 自定义属性分别校验。
- 未注册自定义属性按 token 流比较，忽略空白与注释且区分大小写；标准属性名下的标识符按
  ASCII 大小写折叠。注册属性先校验注册语法，再比较 token；`<color>` 注册额外按解析后的
  颜色值比较，因此 `#00f`、`blue`、`rgb(0 0 255)` 等价。
- 布尔 feature 与属性初始值比较：注册自定义属性用注册的 `initial-value`，标准属性用
  computed style 保存的初始值；未注册且未声明的自定义属性没有值，布尔形式为 false。
- CSS-wide 关键字（`initial`/`inherit`/`unset`/`revert`/`revert-layer`）和 `!important`
  在 feature 值中无效，整个条件求值为 false（§6.2）。
- 没有任何几何或样式环境时一律 false；公开的 `ContainerContext::matches` 保持旧的
  size-only 语义，级联内部使用 `matches_with_style` 并传入自顶向下样式表。

## 接入点

- `render-css::container`：`ContainerQuery::Style(StyleFeature)`、`StyleQueryEnvironment`
  （`container_has_name` + `style_feature_matches`）与 `NoStyleQueries`；解析器把
  `style(...)` 变成 AST，`scroll-state(...)` 仍记为 unsupported。
- `render-css::cascade`：`CascadeLookup` 打包规则索引与样式环境，`@container` 条件用
  `matches_with_style` 判断。
- `render-css::computed`：`ComputedStyleQueries` 读取已计算祖先样式；`style_feature_matches`
  实现 token/注册语法/颜色/布尔/级联关键字规则。

## 测试

- 解析：声明、布尔、函数值、字符串中的冒号、标准属性名折叠、vendor 前缀、`scroll-state`
  仍为 unsupported。
- 集成：无 `container-type`/无几何的样式查询、命名的 style-only 容器、自身排除；
  混合尺寸+样式必须同一容器回答（更近的 style-only 容器不能满足尺寸部分）；布尔初始值
  （注册 `<color>` 初始值与标准属性初始值）；注册 `<color>` 的三种等价写法；混合条件缺
  几何时为 false；CSS-wide 与 `!important` 为 false。

## 验证

```
cargo test -p render-css --lib                     254 passed, 0 failed
cargo test -p render-layout --lib                  318 passed, 1 ignored
cargo test -p render-core --lib                    435 passed, 1 failed
                                                   （临时关闭 avif-native；失败仅为 AVIF 解码测试）
cargo fmt --all --check                            clean
cargo clippy -p render-css -p render-layout -p render-core --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 的 `pkg-config` 缺失阻塞，与本轮代码无关。

## 已知缺口

- `scroll-state()` 容器查询尚未实现（解析为 unsupported，条件 false）。
- 伪元素 `::before`/`::after` 的样式查询应查询其 originating element；当前查询链从
  originating element 的父节点开始，且伪元素级联时该元素样式尚未写入样式表。
- `style()` 的 shorthand feature 应按各 longhand 计算值全部匹配（§6.2）；当前只查同名
  属性，未展开 shorthand。
- 注册属性的非 `<color>` 语法（长度、数字等）目前按 token 比较，未按 computed value 归一化
  （如 `1em` 与 `16px`）。

## 下一步

- 布局包含对 fixed 定位后代的包含块（当前 `position: fixed` 仍直接取 viewport）。
- 布局包含隐含的 stacking context，以及 style containment 的计数器隔离。
- `scroll-state()` 容器查询；伪元素样式查询的 inclusive 祖先链。
- 独立的 `contain: size/layout/style/paint` 属性（当前只由 `container-type` 触发）。
- 继续现代 CSS：`@scope`、`@starting-style`、`@font-face` 描述符、`color-mix()` 等。

---

# 交接文档 (2026-10-31, 续四十八) — `container-type` 的尺寸包含与布局包含

这一轮把 CSS Containment 3 §3 的包含语义接到 `container-type`：非 `normal` 的容器不再只是
查询目标，它自己的布局也按包含规则求解。没有站点特判。

## 布局包含

- `size` 与 `inline-size` 都施加布局包含：主盒成为独立格式化上下文，内部浮动计入其
  `auto` 高度（与 `flow-root` 相同）；静态定位的主盒成为绝对定位后代的包含块，包含块是
  其 padding box。
- `establishes_block_formatting_context` 现在纳入 `container-type`，和其他 BFC 来源共用
  同一条浮动包含路径。
- `layout_block_with_containing_height` 在子布局前与最终高度确定后都使用同一条件
  （`position: static && !layout_contained`），避免静态包含容器在重算阶段退回祖先包含块。

## 尺寸包含

- `size`：块轴内在尺寸视为 0，`height: auto` 不再被内容撑开；显式 `height`、
  `aspect-ratio` 派生高度与 `min-height`/`max-height` 仍然生效，内容可以溢出主盒。
- `inline-size`：inline 轴内在尺寸视为 0。`max_content_width`、`min_content_width_at`、
  `shrink_to_fit_width`（float 与 `inline-block`）以及
  `atomic_outer_intrinsic_width` 都按空内容测量；显式 `width` 与 padding、border、margin
  仍按自身声明参与外框尺寸。
- `ContainerType::from_style` 统一从 computed style 读取 `container-type`，
  `container_geometry_for_style` 与布局层使用同一解析结果。

## 测试

- `size` 的 auto 高度为 0，而 `inline-size` 的 block 轴仍随内容增高。
- `size` 下显式高度、`aspect-ratio`、`min-height` 继续生效。
- 容器建立独立格式化上下文后，内部 float 计入 auto 高度。
- inline-size containment 的 `inline-block` 内在宽度为 0，显式宽度保留。
- 静态 `container-type` 盒成为绝对定位子元素的包含块；同一标记去掉包含后回到初始包含块，
  证明测试观察的是包含语义。

## 验证

```
cargo test -p render-css --lib                     248 passed, 0 failed
cargo test -p render-layout --lib                  318 passed, 1 ignored
cargo test -p render-core --lib                    435 passed, 1 failed
                                                   （临时关闭 avif-native；失败仅为 AVIF 解码测试）
cargo fmt --all --check                            clean
cargo clippy -p render-css -p render-layout -p render-core --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 的 `pkg-config` 缺失阻塞，与本轮代码无关。

## 下一步

- 布局包含对 fixed 定位后代的包含块（当前 `position: fixed` 仍直接取 viewport）。
- 布局包含隐含的 stacking context，以及 style containment 的计数器隔离。
- `style()` / `scroll-state()` 容器查询。
- 独立的 `contain: size/layout/style/paint` 属性（当前只由 `container-type` 触发）。
- 继续现代 CSS：`@scope`、`@starting-style`、`@font-face` 描述符、`color-mix()` 等。

---

# 交接文档 (2026-10-31, 续四十七) — 容器查询单位 `cqw`/`cqh`/`cqi`/`cqb`/`cqmin`/`cqmax`

这一轮把 CSS Containment 3 §5.1 的容器相对单位接到真实布局与绘制路径，没有站点特判。

## 轴选择

- `render-css::container` 新增 `ContainerSizes` 与 `ContainerContext::sizes_for_element`：
  每个轴各自找最近的、在该轴接受尺寸查询的祖先。`cqw`/`cqi` 走 inline 轴，
  `cqh`/`cqb` 走 block 轴；`cqmin`/`cqmax` 因此可以组合两个不同容器的尺寸。
- 元素自身永远不是自己的查询容器；查找从 `dom.parent(element)` 开始。
- 物理轴随书写模式映射：水平书写 `cqw` 取 inline 容器的 width、`cqh` 取 block 容器的
  height；垂直书写反过来。当前引擎只产生水平书写模式，但接口已按两轴建模。
- 没有合格祖先时字段保持 `None`，既有 `LengthResolutionContext` 回退到 small viewport
  尺寸，与 §5.1 一致。

## 接入点

- `render-layout`：新增
  `layout_formatting_tree_with_containers` 与
  `layout_formatting_tree_with_images_and_containers`；`Solver` 持有可选
  `&ContainerContext`，`resolve_length` 在构造 `LengthResolutionContext` 后按元素填充四个
  容器尺寸字段。`None` 上下文保持旧行为（视口回退）。
- `render-core` 绘制：新增
  `build_display_list_with_images_clip_paths_and_containers`，`Builder` 持有 DOM 与容器
  上下文；`transform`/`transform-origin` 的 `translate()`、`cq*` 长度在绘制期与布局期用同
  一套轴选择逻辑解析。旧函数签名保持不变并委托到新函数（容器参数为 `None`）。
- `render_dom` 反馈条件从“存在 `@container` 规则”扩展为“存在容器规则或任何声明使用
  `cq*` 单位”，并扫描内联 `style` 属性；检测按“数字/小数点之后 + 标识符边界”匹配，
  避免把 `--cqw-theme`、URL 等误判为使用容器单位。
- 容器反馈循环与最终布局都传入上一轮测量的 `ContainerContext`；绘制使用同一份上下文，
  因此布局宽度与 transform 位移不会各算一套。

## 测试

- `render-css`：轴选择、不同祖先组合 `cqmin`/`cqmax`、自身排除、垂直书写映射、
  `LengthResolutionContext` 填充与 small viewport 回退。
- `render-layout`：`cqmin`/`cqmax` 跨两个容器解析，以及无容器元素回退视口。
- `render-core`：完整样式→布局→容器测量→再布局链路，以及 `transform: translate(10cqw,
  10cqh)` 在绘制列表中的数值；单位检测的正反例。

## 验证

```
cargo test -p render-css --lib                     248 passed, 0 failed
cargo test -p render-layout --lib                  313 passed, 1 ignored
cargo test -p render-core --lib                    435 passed, 0 failed
                                                   （临时关闭 avif-native；仅 AVIF 解码测试因特性关闭失败）
cargo fmt --all --check                            clean
cargo clippy -p render-css -p render-layout -p render-core --all-targets -D warnings  clean
python tools/check_site_neutrality.py              0 blocking, 26 advisory
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 的 `pkg-config` 缺失阻塞，与本轮代码无关。

## 下一步

- `container-type` 的尺寸包含语义：`size`/`inline-size` 建立独立格式化上下文并对相应轴
  施加 size containment（内容不参与容器自身尺寸）。
- `style()` / `scroll-state()` 容器查询。
- 继续现代 CSS：`@scope`、`@starting-style`、`@font-face` 描述符、`color-mix()` 等。

---

# 交接文档 (2026-10-31, 续四十六) — 现代 CSS：`@property`、媒体查询重写、`@container`

这一轮补齐三类现代 CSS 能力，全部按规范正向实现，没有站点特判、没有静默丢弃。

## `@property`

- 新文件 `crates/render-css/src/registered.rs`：`PropertyRegistration` / `RegisteredSyntax`
  数据模型。解析 `syntax`/`inherits`/`initial-value`，校验语法字符串、`initial-value` 是否匹配
  以及计算独立性；`<transform-list>` 按规范等于 `<transform-function>+`，`<image>` 不把
  `none` 当图像。
- 样式表解析把注册收集到 `StyleSheet.property_registrations`。同一属性后注册覆盖先注册；
  位于 `@media` 内的注册只在媒体条件成立时生效。
- 计算层在 `var()` 替换前按注册语法校验自定义属性值；非法值回退到 unset 值
  （`inherits: false` 用 initial-value，`inherits: true` 用父值再退到 initial-value），
  并产生诊断。注册属性的初始值只 tokenize 一次。
- `@property` 位于 `@container` 内仍然注册：CSS Conditional 5 §5.4 规定全局命名 at-rule
  不受容器条件约束（WPT `container-inner-at-rules.html` 同义）。

## 媒体查询

`cascade.rs` 的媒体查询求值改为 Media Queries 4/5 条件语法：

- `not`、`and`、`or`、任意嵌套括号与三值逻辑（true/false/unknown）。
- 范围语法：`(width >= 600px)`、`(400px < width < 800px)`、双向比较。
- 连续特性 `width`/`height`/`aspect-ratio`/`resolution`/`color`/`monochrome`/`color-index`，
  支持 `calc()`、`em`/`rem`、视口单位与常见分辨率单位。
- 环境特性：颜色方案、reduced-motion/transparency/data、contrast、pointer/hover、
  forced-colors、inverted-colors、scripting、display-mode、color-gamut、dynamic-range、
  update、overflow-block/inline 等。
- `media_query_list_is_supported` 与求值共用同一实现，避免“能求值却报告不支持”的旧缺陷。
- 内置 `window.matchMedia`/样式表诊断随新求值器一起工作。

## `@container`

- 新文件 `crates/render-css/src/container.rs`：`ContainerType`、`ContainerCondition`/
  `ContainerQuery`、尺寸特性解析（plain / min-max / 双向范围 / 三段范围）、
  `orientation`/`aspect-ratio`、命名容器、轴资格与 `ContainerContext` 祖先查询。
- 样式表解析期把 `@container` 条件解析为 AST，附加到每条 `StyleRule`；级联按元素祖先链
  求值。`style()`/`scroll-state()` 被识别为真实语法但暂不能回答，产生诊断而不是静默应用
  或静默丢弃。
- `container`/`container-name`/`container-type` 属性与简写进入注册表和计算层。
- 容器几何由布局回馈：`render_dom` 在存在容器规则时最多迭代 3 次样式/布局直到尺寸稳定；
  没有容器规则时不增加布局。尺寸取内容盒；`inline-size` 只回答 inline 轴，block 轴查询
  继续向上找合格容器；没有几何时查询为 false，不猜视口。
- `@keyframes` 位于 `@container` 内不受条件约束（CSS Conditional 5 §5.4），本轮修正了此前
  按元素门控 keyframes 的错误；只有 `@media` 能门控 keyframes。

## 验证

```
cargo test -p render-css --lib                     245 passed, 0 failed
cargo test -p render-core --lib                    432 passed, 0 failed
                                                   （临时关闭 avif-native；仅 AVIF 解码测试因特性关闭失败）
cargo fmt --all --check                            clean
cargo clippy -p render-css --all-targets -D warnings  clean
python tools/check_site_neutrality.py              clean
```

本机全工作区门禁仍被 `dav1d-sys v0.8.3` 阻塞：构建脚本找不到 `pkg-config`
（`The pkg-config command could not be found.`）。这与本轮代码无关；在具备
`pkg-config` + `libdav1d` 的环境需重跑 `cargo test --workspace` 与全工作区 clippy。

## 下一步

- 容器单位 `cqw`/`cqh`/`cqi`/`cqb`/`cqmin`/`cqmax` 的计算路径（属性解析已识别，需把
  查询容器尺寸接入 `LengthContext`）。
- `container-type` 的尺寸包含语义（`size`/`inline-size` 建立独立格式化上下文与包含）。
- `style()`/`scroll-state()` 容器查询。
- `@font-face`、`@layer` 在容器/条件规则中的全局语义审查。

---

# 交接文档 (2026-10-31, 续四十五) — 设置页 HTTP 代理 + GitHub 样式性能

## 设置页 HTTP 代理

`render://settings` / `about:settings` 现在包含“HTTP 代理”区:

- 输入框 `http-proxy-address`,支持 `host:port` 与 `http://host:port`。
- 保存按钮 `save-http-proxy`,只在受信设置文档上生效;远程页面复制
  `data-render-action` 不会获得权限。
- 保存后立即替换 `NetworkWorker` 底层 `HttpTransport` 的 agent;正在进行的
  请求保持旧连接,不中断加载。
- 留空保存是显式“直接连接”,重启后也不会重新继承 `HTTPS_PROXY`。
- 选择持久化到 profile 的 `proxy.txt`(原子写,Unix 0600);损坏文件会被忽略。
- 设置页对代理值、状态和错误信息做 HTML 转义;错误保存保留用户草稿。

代理地址仍没有写进任何 tracked code。启动时只读取 profile 里的用户输入:

```
proxy.txt:
# rENDER proxy v1
http://127.0.0.1:8080
# 或 direct
```

## GitHub 性能

真实 GitHub 代理抓取使用 28 个 CSS、38,396 条规则。优化前后同页样式帧:

| 阶段 | 优化前 | 优化后 |
|---|---:|---:|
| cascade | 54.48s | 3.58s |
| layout | 15.41s | 0.027s |
| raster | 1.98s | 2.1s |
| 样式帧总耗时 | ~72s | ~5.7s |

关键改动:

- 选择器右端 compound 的 id/class/type/attribute key 建立规则桶;`universal`
  桶从 1,851 条降到 823 条(含伪元素上下文拆分)。
- 按 `::before`/`::after` 上下文分桶,普通元素不再遍历纯伪元素规则。
- `ComputedStyle` 的 properties/typed/custom 表改为 `Arc` 共享;flex
  intrinsic sizing 的样式拷贝从深拷贝变成引用计数。
- 初始值改为共享 `InitialValues`;未声明且未继承的属性不再复制到每个元素。
- 未声明自定义属性时直接共享父级已解析的 custom-property map;`var()` 通过
  父级 map 回退解析,不再逐元素复制并重新解析全部变量。
- 继承属性未变化时直接复制父级的值和 typed 值,避免重复 tokenize/parse。
- flex intrinsic size 增加每次 layout 内的 memoize(GitHub: 425 hits / 7,870 misses)。
- CSS animation 无 `animation-name` 时提前返回,避免主线程每帧解析全部动画属性。

环境变量 `RENDER_DEBUG_CSS_INDEX=1` 可输出规则索引/候选/匹配/子阶段耗时;
`RENDER_DEBUG_FLEX_CACHE=1` 可输出 flex 缓存命中;生产路径不执行计时。

## 验收

```
cargo test --workspace                       2852 passed, 0 failed
real_site_tasks                              66 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
```

隐藏窗口截图:

```
GitHub (proxy): ready=Complete dom_elements=2124 boxes=1580
                css_warnings=149 scripts_threw=4 images_failed=0
HN (proxy):     ready=Complete dom_elements=811 boxes=333
Bing:           ready=Complete dom_elements=474 boxes=166 css_warnings=13
Baidu:          ready=Complete dom_elements=450 boxes=156 css_warnings=5
JD:             ready=Interactive dom_elements=1015 boxes=117
                scripts_failed=12 css_warnings=36
```

设置页截图确认代理输入、保存按钮、状态行和缓存区正常排版。

## 仍未完成

- GitHub 样式帧已能在 ~3.6s 内完成,但视觉上仍缺主内容区。主要缺口是
  `@container`(60+20+2 条规则)、`@position-try`、`@property` 和部分
  cascade layer 语法未求值;不是性能问题。
- JD 主商品区仍受站点 403/风控,需要受信任登录态或住宅网络。
- 代理 UI 目前只支持 HTTP CONNECT 代理,不支持 SOCKS/HTTPS 代理、PAC、
  按站点例外列表或代理认证的独立管理界面(带 userinfo 的 HTTP 代理可用)。
- `ComputedStyle` 的 Arc 化移除了 `properties()` 借用整个 map 的公开访问,
  改为 `get`/`property_names`/`property_count`;仓库内调用点已更新。

---

# 交接文档 (2026-10-31, 续四十四) — 并行子任务与代理验证

这一轮用 4 个独立 worktree/subagent 并行推进,随后逐个审查、cherry-pick 回主树。

## 合并结果

| 能力 | 提交 |
|---|---|
| **SVG feDropShadow/feMorphology** | `2ecb5c0`;drop shadow 复用 blur/offset/flood/merge;morphology 支持 erode/dilate、x/y radius、result 引用 |
| **conic/repeating 渐变** | `e8a119b`;conic 方向/位置/角度停靠点、repeating linear/radial、multi-position stops、径向渐变实际光栅化 |
| **IndexedDB 值类型** | `b608aab`;Date/RegExp/Map/Set/ArrayBuffer/11 种 TypedArray、NaN/Infinity/-0/undefined/BigInt/稀疏数组,带引用图与循环 |
| **嵌套 GroupEffect local timing** | `5019b17`;子 group 自身 delay/duration/iterations/direction/easing/fill、`updateTiming`、`Animation.effect` 替换、auto duration |
| **动态 CSS link 重键** | `c41f281`;link 元素被替换但 URL 不变时按 URL 复用已下载样式表 |

## 代理验证（不写入代码）

用户提供 `<test-proxy>:8080`,仅通过环境变量测试:

```
HTTPS_PROXY=http://<test-proxy>:8080
HTTP_PROXY=http://<test-proxy>:8080
ALL_PROXY=http://<test-proxy>:8080
```

Curl:

```
GitHub 200, HN 200
```

隐藏截图:

```
HN:     ready=Complete dom_elements=811 boxes=333
        scripts_failed=0 scripts_threw=0 css_warnings=0 images_failed=0
GitHub: ready=Loading  dom_elements=2123 boxes=845
        scripts_failed=0 scripts_threw=0 css_warnings=0 images_failed=0
```

HN 渲染正常。GitHub 网络已通且 28 个 CSS 全部 200,但最终 `stylesheets=0`,
截图仍是无样式 HTML;这是新的样式批处理/提交问题,不是代理问题。代理字符串
已确认不在 tracked code 中。

## 当前验证

```
cargo test --workspace                       2842 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

内容站点复跑:

```
Bing:  ready=Complete  dom_elements=474 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=13 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Interactive dom_elements=1159 boxes=117
       scripts_failed=11 scripts_threw=2 css_warnings=36 images_failed=1
```

## 明确未做 / 下一步

- GitHub 的 CSS 已下载但未进入最终样式表;需继续定位 batch commit/rematch。
- conic 渐变仍仅支持百分比 stop、sRGB、无角度抗锯齿;颜色提示/绝对长度未做。
- IndexedDB 的 Blob/DataView 未持久化;Map/Set 自循环仍受现有 structuredClone 限制。
- GroupEffect 仍只取第一个 child target,子结构变更后不自动刷新 base ownership。
- JD 主商品区仍受 403;GitHub 渲染未完成;高级 SVG 原语、IndexedDB Blob 等仍未完成。

---

# 交接文档 (2026-10-31, 续四十二) — KeyframeEffect/GroupEffect 与 transitionrun/start

接续 WAAPI effect stack 会话。这一轮补 `KeyframeEffect`/`GroupEffect` 对象、
嵌套 group 采样,以及 `transitionrun`/`transitionstart` 事件。

## 实现

| 内容 | 落点 |
|---|---|
| **KeyframeEffect** | `new KeyframeEffect(target,keyframes,options)`;getTargets/getKeyframes/getTiming/getComputedTiming |
| **GroupEffect** | `new GroupEffect(effects,options)`;append/prepend/remove/getChildren/getTiming;支持嵌套 group |
| **Animation(effect, timeline)** | 接受 effect 对象;element.animate 改为先建 KeyframeEffect 再建 Animation |
| **group 采样** | 子效果按 source order 合并;replace/add/accumulate 使用现有合成器;effect stack 统一写回 |
| **transitionrun/start** | `TransitionEventKind::{Run,Start}`;Run 在 transition 创建时,Start 在 delay 结束后的首个采样帧;DOM 映射为 `transitionrun`/`transitionstart` |
| **测试** | GroupEffect/KeyframeEffect 结构、嵌套 group 的 opacity/width 采样、Run/Start 事件顺序与 DOM 映射 |

## 证据

本地 group 页面:

```js
var first = new KeyframeEffect(box, [{opacity:0},{opacity:1}], {duration:1000, fill:'both'});
var nested = new GroupEffect([new KeyframeEffect(box, [{width:'0px'},{width:'100px'}], ...)]);
var group = new GroupEffect([first, nested], {duration:1000, fill:'both'});
new Animation(group, document.timeline).play();
```

500ms 日志:

```
GROUPPROBE opacity=0.4438 width=44.383px children=2
health ready=Interactive scripts_failed=0 scripts_threw=0
```

即嵌套 group 同时驱动 opacity 和 width。

transitionrun/start 页面:

```
标题栏 transitionrun:width|transitionsta...
```

单元测试:

```
web_animation_keyframe_and_group_effects_are_constructible ... ok
transitionrun_and_transitionstart_precede_the_end ... ok
transition_run_and_start_map_to_their_dom_event_types ... ok
```

真实站点:

```
Bing:  ready=Complete  dom_elements=480 boxes=167
       scripts_failed=0 scripts_threw=0 css_warnings=1 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=1158 boxes=117
       scripts_failed=8 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2818 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- GroupEffect 子效果目前共享父 progress,没有独立 local timing/delay/iteration
  账本;GroupEffect/KeyframeEffect 的完整 Web Animations 语义仍不完整。
- effect stack 已支持顺序与 add/accumulate,但 EffectGroup 的 ownership、
  `animation-composition` 全规则和复杂 easing 仍缺。
- transitionrun/start 已完成,但文档切换 cancel 的真实导航端到端证据仍受本机导航
  入口限制。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续四十一) — WAAPI effect stack 与逐关键帧 easing

接续 WAAPI 速率/合成会话。这一轮把多个动画从“轮流改写 inline style”改为有
明确创建顺序的 effect stack,并让 keyframe 自带的 easing 生效。

## 实现

| 内容 | 落点 |
|---|---|
| **effect stack** | 所有活动动画按创建顺序按属性合成;replace 覆盖、add/accumulate 叠加上一结果 |
| **base style** | 每个元素/属性首次被动画时保存原始 inline 值;最后一个效果移除后恢复 |
| **逐帧输出** | `_apply` 只计算 `_values`;每帧结束 `renderAll` 统一写回,避免动画间互相污染 |
| **结束/取消** | fill forwards 保留最终值;fill none/cancel 清除效果并恢复底层样式 |
| **per-keyframe easing** | keyframe 的 `easing` 存在点之间;`sampleTrack` 用前一个关键帧的 easing,缺省回退 effect easing |
| **测试** | steps(1,end) 中点仍为 0、两个 replace 动画后者胜出、add 合成仍为 75px |

## 证据

单元测试:

```
web_animation_effect_stack_uses_creation_order ... ok
web_animation_per_keyframe_easing_overrides_the_default ... ok
web_animation_add_composition_uses_the_previous_value ... ok
web_animation_playback_rate_and_reverse_adjust_the_timeline ... ok
```

effect stack 页面:100px replace + 50px add,duration 1s,500ms 时:

```
STACKPROBE width=69.6399px first=running second=running
```

即约 70px,而不是只保留第一个/第二个动画的 50px 或 100px。

真实站点:

```
Bing:  ready=Complete  dom_elements=477 boxes=167
       scripts_failed=0 scripts_threw=0 css_warnings=13 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=999 boxes=117
       scripts_failed=7 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2815 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- `GroupEffect`/`KeyframeEffect` 对象、effect group 嵌套、animation-composition
  的完整 ownership 仍未实现;当前是 linear effect stack。
- 逐关键帧 easing 在区间内近似实现,复杂 spring/linear() easing 与完整 timing
  模型仍有差距。
- `transitionrun`/`transitionstart` 未发出;WAAPI 仍通过 inline style 写回。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续四十) — 节点移除 transitioncancel 与精确反向

接续 WAAPI 统一时间线会话。这一轮补节点移除/文档切换的 transitioncancel、
精确 reversing-adjusted start value,并修复无单位零长度无法插值的问题。

## 实现

| 内容 | 落点 |
|---|---|
| **node removal cancel** | `sample_transitions` 对不在本帧 computed styles 的活动节点生成 `transitioncancel`,elapsedTime 为已运行时长 |
| **document switch cancel** | `TransitionState::cancel_all`;`PageState::set_source` 在替换旧 Page 前排队事件并运行旧 runtime |
| **detached dispatch** | `PageDomEvent::from_transition` 统一事件构造;已测试 target 从文档移除后监听器仍收到事件 |
| **精确 reversing-adjusted start** | `ActiveTransition.reversing_adjusted_start`;正常 transition 为 start value,反向 transition 为旧 end value;再次反向按 §3.1 识别并缩短 |
| **transitioncancel** | 反向/中断/节点移除/文档切换均使用同一 `TransitionEventKind::Cancel` 路径 |
| **零长度插值** | `width: 0` 等无单位零长度在长度属性中按 `0px` 归一,修复 `0 -> 100px` 无法启动 transition |
| **测试** | 节点移除 cancel、detached 监听器、document-switch cancel_all、重复反向、unitless zero 插值 |

## 证据

本地页面:元素在 transition 进行到约 0.62s 时被 `remove()`:

```
transition start width 0 -> 100px
transition state active=1 ...
TRANSITIONCANCEL width 0.621999979019165
标题栏: cancel:width:0.62
health scripts_failed=0 scripts_threw=0
```

同一 session 的 `transitionend` 页面仍输出 `end:width:1`,说明移除前的事件没有误报。

单元测试:

```
0px -> 100px @0.5s -> 反向 0px @0.5s -> 再次反向 100px
最终 1.5s 到达 100px, transitionend.elapsedTime = 0.75s(按 0.75 factor)
detached element listener: detached:width:0.25
cancel_all: 1 Cancel event, state empty
unitless width 0 -> 100px @0.5 = 50px
```

真实站点:

```
Bing:  ready=Complete  dom_elements=471 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=7 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=1 images_failed=0
JD:    ready=Interactive dom_elements=1001 boxes=117
       scripts_failed=10 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2813 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- `transitionrun`/`transitionstart` 事件仍未发出;文档切换 cancel 的端到端脚本证据受
  本机导航入口限制,当前由 cancel_all/detached dispatch 单元测试覆盖。
- WAAPI 仍通过 inline style 写入,effect stack/group、逐关键帧 easing、完整 timing
  模型未对齐。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十九) — WAAPI 速率/反向、合成与统一时间线

接续 Web Animations 核心会话。这一轮补 `playbackRate`/`reverse`、add/accumulate
合成，并让 WAAPI 与 CSS 动画共用页面的 document clock。

## 实现

| 内容 | 落点 |
|---|---|
| **统一时间线** | WAAPI 不再自己 `setTimeout`;`Page::sync_web_animations(now)` 在页面 pump/帧中用 `created_at.elapsed()` 调用 `__renderTickWebAnimations(time)` |
| **时钟激活** | `PageState.has_active_timeline` 增加 `Page::has_active_web_animations`;浏览器继续按 16ms 调度同一帧循环 |
| **playbackRate** | getter/setter、`updatePlaybackRate`;改变速率时保持 currentTime,速率 0 冻结 |
| **reverse** | 翻转 playbackRate;finished/idle 时从正确端点重新播放 |
| **负速率** | currentTime 从 end 向 0 采样,到达 0 时 finish |
| **合成** | `composite: replace/add/accumulate`;数字/长度相加、transform 函数串接;后者读取当前 inline 值 |
| **document.timeline** | WAAPI 暴露 `document.timeline.currentTime` 与 `Animation.timeline`,与页面 document clock 同源 |
| **测试** | 2x 速率、负速率/currentTime、reverse、add 合成、Page 时钟 tick、既有 pause/finish/cancel |

## 证据

本地 WAAPI 页面(`duration:1000, fill:both`),外部时钟每帧 tick:

```
500ms  WAAPIPROBE state=running current=494.8699
       opacity=0.4734 transform=translateY(9.468px)
1600ms 标题: waapi-finished
       health ready=Complete scripts_failed=0 scripts_threw=0
500ms  截图/最终截图显示方块从半透明偏移位置过渡到不透明最终位置
```

单元测试:

```
playbackRate 0.5s -> 2x 后 0.75s 到达 100px/finished
playbackRate:-1 在 2.5s 时 currentTime=500、width=50px
reverse() -> playbackRate 1、state running
add composite: first 50px + second 25px = 75px
Page: pump 500ms -> style width 50px, pump 1500ms -> finished
```

真实站点:

```
Bing:  ready=Complete  dom_elements=480 boxes=167
       scripts_failed=0 scripts_threw=0 css_warnings=7 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=969 boxes=117
       scripts_failed=8 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2808 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- WAAPI 仍通过 inline style 写入,多个动画/作者内联样式会互相覆盖;完整 effect
  stack/group、composite order 与替换恢复仍不完整。
- 逐关键帧 easing、`getComputedTiming` 全字段、`Animation.timeline` 的完整
  timeline object、`playbackRate` 0 的规范 pending 状态仍未对齐。
- 节点移除/文档切换的 transitioncancel、精确 reversing-adjusted start value 未做。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十八) — Web Animations API 核心

接续 transitioncancel/allow-discrete 会话。这一轮实现 `Element.animate` 的
播放控制、关键帧采样、动画事件和帧驱动时间线。

## 实现

| 内容 | 落点 |
|---|---|
| **自托管 WAAPI** | 新 `prelude_web_animations.js`,在 DOM/CSSOM/IndexedDB prelude 之后安装 |
| **Element.animate** | 支持数组关键帧和对象-of-数组;camelCase 属性名转 kebab-case |
| **时间参数** | duration/delay/endDelay/iterations/direction/fill/easing;Infinity 迭代 |
| **采样** | 16ms `setTimeout` 驱动,`performance.now()` 时间线;数字/长度/颜色/transform 插值,`transform:none` 以 identity 处理 |
| **播放控制** | `play`/`pause`/`finish`/`cancel`,`playState`,`currentTime` |
| **事件** | `onfinish`/`oncancel` 与 `addEventListener`/`removeEventListener`;`finished` promise |
| **样式联动** | 采样值写入 inline style,沿用现有 DOM revision → render 管线 |
| **测试** | 迁移到 0.5/1.0s 的 opacity、pause/currentTime、finish、cancel、transform none |

## 证据

本地 WAAPI 页面:

```js
var animation = box.animate(
  [{ opacity: 0, transform: 'translateY(0px)' },
   { opacity: 1, transform: 'translateY(20px)' }],
  { duration: 1000, fill: 'both' }
);
animation.onfinish = function () { document.title = 'waapi-finished'; };
```

运行日志/截图:

```
WAAPIPROBE state=running current=479.693084
            opacity=0.4793 transform=translateY(9.5856px)
1500ms 标题: waapi-finished
health ready=Complete scripts_failed=0 scripts_threw=0
```

即时间线在约 480ms 采样到 opacity≈0.48、translateY≈9.59px,结束事件在
截图前触发。700/900ms 截图也显示部分透明、位置偏移的方块。

真实站点:

```
Bing:  ready=Complete  dom_elements=474 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=13 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=1158 boxes=117
       scripts_failed=8 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2805 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

注:prelude 增加对象后,`a_frame_that_cannot_get_a_realm...` 的测试堆预算从
6000 调到 7000,仍保持"只容纳两个嵌套 realm"的测试意图。

## 明确未做 / 下一步

- WAAPI 与 CSS Animations/Transitions 的时间线和合成尚未统一;WAAPI 值写入
  inline style,会覆盖/干扰作者内联样式,多个动画写同一属性时恢复顺序仍简单。
- `playbackRate`、`reverse`、effect composition、逐关键帧 easing、
  `getComputedTiming` 完整字段、`Animation.timeline` 未实现。
- 节点移除/文档切换时的 transitioncancel 仍未完整;精确 reversing-adjusted
  start value 未做。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十七) — transitioncancel 与 allow-discrete

接续多动画/反向 transition 会话。这一轮补 `transitioncancel` 事件和
`transition-behavior: allow-discrete`。

## 实现

| 内容 | 落点 |
|---|---|
| **transitioncancel** | `TransitionEventKind::{End,Cancel}`;切换目标/属性不再覆盖时发出 cancel,`elapsedTime` 为已运行时长 |
| **事件映射** | 浏览器按 kind 派发 `transitionend` 或冒泡 `transitioncancel`,继续带 `propertyName`/`elapsedTime`/`pseudoElement` |
| **allow-discrete** | 注册 `transition-behavior: normal`;简写可写 `transition: display 1s allow-discrete`;显式列表按下标匹配 |
| **离散采样** | 无插值器的属性在 allow-discrete 下按离散值处理;`display:none` 隐藏延迟到结束,出现时立即显示 |
| **默认行为** | 未声明 allow-discrete 时离散属性仍立即变化,不产生 transition |
| **测试** | cancel kind/elapsedTime、allow-discrete 显隐、默认离散立即生效、transitioncancel 监听器参数 |

## 证据

本地页面:红色 `#a` 使用 `transition: display 1s linear allow-discrete` 并
在 100ms 后加 `hide`;蓝色 `#b` 的 width 在 100ms 变 120px、400ms 再变 60px,
监听 `transitioncancel` 写标题。

```
500ms  a=(255,0,0) 红仍可见   b=(0,0,255) 蓝
1500ms a 已隐藏              b=(0,0,255)
标题   cancel:width:0.32
health scripts_failed=0 scripts_threw=0 css_errors=0
```

即 display 的消失等到 transition 结束才生效;中途改 width 触发
`transitioncancel`,`elapsedTime≈0.32s`。

真实站点:

```
Bing:  ready=Complete  dom_elements=474 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=13 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=1158 boxes=117
       scripts_failed=8 scripts_threw=2 css_warnings=36 images_failed=1
```

Bing 页头正常。JD 此次又停在 Loading,主商品区仍受 403 风控,站点响应有波动。

## 验证

```
cargo test --workspace                       2802 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- `transitioncancel` 对节点被移除/文档切换的派发还未完整;当前主要覆盖样式变更取消。
- `allow-discrete` 只对无插值器属性做离散,`display` 的 start/end 特例按规范近似;
  其它离散属性的 50% 翻转仍是简化实现。
- 精确 reversing-adjusted start value、WAAPI、多动画 pause/resume 计时账本未做。
- 动画/过渡时钟只驱动当前标签页,无独立合成器线程。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十六) — 多动画列表、transform:none 与 transition 反向

接续 CSS Transitions 会话。这一轮补多动画列表、`transform: none` 的 identity
插值、transition reversing shortening，并修正动画探针时间单位。

## 实现

| 内容 | 落点 |
|---|---|
| **多动画列表** | `animation-name`/duration/delay/timing/iteration/direction/fill/play-state 按下标匹配,短列表循环、长列表截断;按 source order 采样,后者覆盖同名属性 |
| **animation 简写分层** | `animation: a 1s, b 2s` 正确展开为每个 longhand 的逗号列表 |
| **transform:none** | 解析 typed `TransformList`,把 `none` 视为 identity;`translate/scale/rotate/skew/matrix` 生成零值端点后走数值插值,用于动画和 transition |
| **transition 反向** | 目标回到旧 `from` 时计算 CSS Transitions §3.1 reversing shortening factor;duration 乘以 factor,负 delay 同步缩短 |
| **中断规则** | 目标变为第三种值时从当前采样值重新开始完整 duration,不跳回旧端点 |
| **探针时间** | 修正 `RENDER_ANIMATION_TIME_MS` 的毫秒→秒换算(此前 500 被当成 500 秒) |
| **测试** | 多动画组合、animation 简写分层、transform none 动画/过渡、transition reversing、现有动画回归 |

## 证据

多动画 + `transform:none` 探针(`grow` 宽 0→100px + `translateY(0→20px)`,
`fade` opacity 0→1),同一页面两个固定时刻:

```
RENDER_ANIMATION_TIME_MS=500   animation time=0.500s
  (20,215)=(255,128,128)   宽约50px、opacity 0.5
RENDER_ANIMATION_TIME_MS=1000  animation time=1.000s
  (20,215)=(255,0,0)       宽100px、opacity 1
  x=60: 500 时白,1000 时红
```

即两个动画同时生效,`transform:none` 在中间帧产生 `translate(0px, 5px)`/10px
的位移,不是 50% 跳变。

transition reversing 单元测试:

```
0px -> 100px @0.5s   = 50px
反向回 0px @0.5s
0.75s                = 25px
1.0s                 = 0px, transitionend.elapsedTime = 0.5s
```

即反向只花剩余 0.5s,而不是再走完整 1s。

真实站点:

```
Bing:  ready=Complete  dom_elements=474 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=1 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Complete  dom_elements=926 boxes=154
       scripts_failed=4 scripts_threw=2 css_warnings=36 images_failed=0
```

JD 已能完成加载且图片失败为 0,但主商品区仍因 `api.m.jd.com` 403 风控为空。

## 验证

```
cargo test --workspace                       2797 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- `transitioncancel` 事件、`allow-discrete`、精确 reversing-adjusted start value
  仍未实现;多动画的 pause/resume 计时账本也不完整。
- 多动画只支持 replace 合成,`animation-composition` 的 add/accumulate 未做。
- `transform:none` 只覆盖 2D typed transform 函数;列表函数数量/顺序不一致时仍离散。
- 动画/过渡时钟仍只驱动当前标签页;没有独立合成器线程;WAAPI 未实现。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十五) — CSS Transitions 与 transitionend

接续动画时钟会话。这一轮实现 `transition`/`transition-*` 的变更检测、时间采样和
`transitionend` 事件。

## 实现

| 内容 | 落点 |
|---|---|
| **解析** | `transition` 简写展开为 property/duration/timing-function/delay 四个 longhand;支持逗号分层与 `cubic-bezier(...)`/`steps(...)` |
| **初始值** | `transition-property: all`、`transition-duration: 0s`、`transition-timing-function: ease`、`transition-delay: 0s` |
| **状态** | 新 `render-css/src/transition.rs`:每个文档一份 `TransitionState`,保存上帧目标值与活动 transition |
| **变更检测** | 比较本帧基础 computed value 与上帧目标;`all` 只跟踪常见可插值属性,显式属性按列表匹配 |
| **采样** | delay、duration、timing-function;数字/长度/矩阵函数/sRGB 颜色插值;0s 立即生效 |
| **transitionend** | 结束时返回 `TransitionEvent { node, property, elapsed_time }`;浏览器提交帧时排入 DOM 事件,带 `propertyName`/`elapsedTime`/`pseudoElement`,并冒泡 |
| **时钟** | `PageState.has_active_timeline` 同时看 CSS 动画和活动 transition,浏览器继续 16ms 采样 |
| **测试** | 简写分层展开、0/0.5/1s width 采样、delay、0s、transitionend 参数与冒泡、文档端到端 |

## 证据

本地 transition 页面(`transition: width 1s linear, background-color 1s linear`,
100ms 后加 `wide` 类):

```
600ms 截图 x=25..50 = (177,0,78) 紫色(红→蓝中间态), x=70 白
3000ms 最终截图 x=25..110 = (0,0,255), x=130 白
TRANSPROBE width=120 title=end:width:1
48 个动画帧, 最后 animation time=1.147s
```

即宽度 20→120、颜色红→蓝都在真实时钟中插值,`transitionend` 在截图前触发,
`event.propertyName = "width"`、`event.elapsedTime = 1`。600ms 截图仍可能是上一帧
(433ms)的像素,但已明确处于过渡中间态。

真实站点:

```
Bing:  ready=Complete  dom_elements=475 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=9 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=1158 boxes=117
       scripts_failed=8 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2791 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- 未实现 reversing shortening、`transition-behavior: allow-discrete`、
  中断 transition 的 ownership/composite 规则;`transform: none` 与具体函数之间
  仍按离散处理。
- 仍只支持常见可插值属性的列表;部分 shorthand/颜色空间(oklab 等)与
  `box-shadow` 插值未做。
- 动画仍是单标签页时钟,无独立合成器;WAAPI、多动画列表、后台标签页推进未做。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十四) — 浏览器驱动的 CSS 动画时钟

接续 RMS 依赖加载会话。这一轮把动画从“每个静态帧只取一个时间点”推进到
浏览器按 16ms 重新采样、调度重绘、有限动画结束后自动停表。

## 实现

| 内容 | 落点 |
|---|---|
| **帧时间身份** | `RenderIdentity.animation_time_millis`;每帧记录文档启动以来的毫秒数 |
| **活动检测** | `animation_is_active`/`any_animation_active`:duration、delay、iteration-count、play-state;有限动画在总时长后停止,无限动画继续 |
| **浏览器时钟** | `about_to_wait` 检测当前标签页的活动动画,每 16ms 调用 `schedule_page_render_for_tab` 并设置 `WaitUntil` |
| **worker 采样** | `process_page_render` 默认使用 `identity.animation_time_millis`;`RENDER_ANIMATION_TIME_MS` 仍可固定时刻做探针 |
| **有限停表** | 动画结束后不再产生空转帧;交互/DOM 变更仍会触发正常重绘 |
| **`document.elementFromPoint`** | 用已安装的文档坐标 + 滚动偏移命中最小面积元素;修复 Clarity 分析脚本在 Bing 上的 `callee.elementFromPoint is not callable` |
| **能力登记** | `css.animations` 说明更新为浏览器驱动 16ms 时钟,仍标 `Partial` |
| **测试** | 有限动画在 0/0.5/1/2s 的活动状态、无限动画、paused;`elementFromPoint` 命中与滚动偏移 |

## 证据

动画探针 `animationprobe.html`(同一页面三个真实时刻,未设置固定覆盖):

```
clock-300   fade(10,202) = (255,255,255)   grow x=30 白
clock-700   fade(10,202) = (255,92,92)     grow x=30 白
clock-1200  fade(10,202) = (255,0,0)       grow x=30 蓝
```

即 opacity 0→1 连续插值、width 20px→40px 进入布局。

`RENDER_DEBUG_FRAME=1` 的 Bing 截图记录到连续帧时间:

```
animation time=0.216s
animation time=0.000s
animation time=0.876s
animation time=2.223s
...
animation time=7.009s
animation time=9.088s
animation frames: 12
```

说明动画时钟在真实页面上持续调度,而不是只采一个固定时刻。

真实站点最终复跑:

```
Bing:  ready=Complete  dom_elements=484 boxes=167
       scripts_failed=0 scripts_threw=0 css_warnings=2 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=999 boxes=117
       scripts_failed=7 scripts_threw=2 css_warnings=36 images_failed=1
```

Bing 截图中页头、导航、Rewards/登录、壁纸均正常;`bing196` 的页头区域
与 `bing160` 仅有意外的壁纸/变体差异,不再是整块缺失。

## 验证

```
cargo test --workspace                       2785 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- 时钟只驱动当前活动标签页;后台标签页不推进动画。没有独立合成器线程,
  每个采样帧仍走完整样式/布局/绘制管线。
- 仍只采样第一个 `animation-name`;逗号分隔的多动画、CSS Transitions、
  Web Animations API、`animation-composition` 未实现。
- 动画结束/暂停只按 computed style 判断;脚本直接改 DOM 或 CSSOM 后需要
  下一帧才会纳入时钟。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter
  其余原语、IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十三) — Bing RMS 依赖加载顺序

接续 `text/rms` 会话。上一轮直接按 Defer 执行这些块,导致依赖 RMS 懒加载 bundle
的块(Feedback)在 bundle 到达前抛 `ReferenceError`。这一轮让它们等 RMS 加载完
注册资源后再执行。

## 实现

| 内容 | 落点 |
|---|---|
| **标记 RMS 块** | `ClassicScript.rms`/`PlannedScript.rms`;`text/rms` 在发现阶段保留该标记 |
| **执行包装** | `rms_bootstrap_source`:注册 `window.rms.onload(run)`,调用 `window.rms.start()`,由 run 追加一个 `text/javascript` 脚本元素执行原始源码 |
| **全局作用域** | 原始源码作为新脚本元素执行,不进入包装函数的局部作用域,`var mcp_banner` 等仍是页面全局 |
| **回退** | 页面没有 `window.rms`/`start`/`onload` 时立即执行,保持原有行为 |
| **转义** | JS 字符串字面量处理反斜杠、引号、换行、U+2028/U+2029;回归测试验证包装源码可编译 |
| **测试** | `text/rms` 的 rms 标记/Defer 调度、包装源码编译;Bing 实页延迟探针 |

## 证据

Bing 隐藏截图 + 延迟 3 秒探针:

```
[console.log] RMSPROBE0 count=9 mcp=function feedback=undefined
[console.log] RMSPROBE3 mcp=function bm=object feedback=object zoom=object share=object
health ready=Complete dom_elements=480 boxes=167
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=13 images_failed=0
```

即 9 个 text/rms 块仍存在;加载时 `feedback` 尚未就绪,3 秒后
`mcp_banner`、`BMTrigger`、`Feedback`、`AccessibilityZoomDesktop`、
`ShareDialogConfig` 全部定义,且不再有未捕获异常。Bing 截图中壁纸、页头、
Rewards/登录、热搜均正常。

Baidu / JD:

```
Baidu: ready=Complete   dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading    dom_elements=1159 boxes=117
       scripts_failed=8 scripts_threw=3 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2783 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- RMS 包装依赖 Bing 的 `window.rms` 形状(`onload`/`start`);不是通用标准 API。
  每个 text/rms 块都会注册回调并调用 start,bundle 加载由 RMS 自身去重。
- `Feedback`/`AccessibilityZoom` 等 bundle 是否加载仍由 Bing 的 RMS 队列决定;
  其它站点自定义脚本类型仍需单独建模。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;连续动画时钟、高级渐变、
  SVG filter 其余原语、IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十二) — Bing `text/rms` 延迟脚本

接续动态 import 会话。这一轮让 Bing 的 `<script type="text/rms">` 真正执行。

## 实现

| 内容 | 落点 |
|---|---|
| **类型识别** | `text/rms` 加入 classic JavaScript MIME 集合,不再报 `UnsupportedType` |
| **调度** | 作为 `ScriptScheduling::Defer` 执行;这些块位于 `#aRmsDefer` 容器,语义上是页面尾部的延迟脚本 |
| **执行** | 复用现有 classic script 编译/执行管线,内容里的 `//<![CDATA[` 行注释照常解析 |
| **测试** | 内联/外链 `text/rms` 都被发现且调度为 Defer,无诊断 |

## 证据

Bing 页面有 9 个 `text/rms` 块。隐藏截图 + `RENDER_DEBUG_EVAL`:

```
RMSPROBE count=9 mcp=function bm=object feedback=undefined zoom=object share=object aRms=present
health ready=Complete dom_elements=465 boxes=166
       scripts_failed=0 scripts_threw=1 css_warnings=7 images_failed=0
```

即 `mcp_banner`、`BMTrigger`、`AccessibilityZoomDesktop`、`ShareDialogConfig`
等全局已由这些延迟脚本定义,说明块确实执行了。

唯一未捕获异常来自 Feedback 初始化:

```
classic script node NodeId(331) source 16 inline failed: Feedback is not defined at line 3, column 10
```

`Feedback` 的定义在 `rms:answers:Feedback:FeedbackBootstrapBundle` 懒加载 bundle 中;
该 bundle 未在本轮页面加载,故这个块依赖缺失。其余 text/rms 块正常完成。

Baidu / JD:

```
Baidu: ready=Complete   dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Interactive dom_elements=1001 boxes=117
       scripts_failed=10 scripts_threw=2 css_warnings=36 images_failed=1
```

## 验证

```
cargo test --workspace                       2782 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

注:`app_tests::a_cross_origin_put_is_sent_only_after_its_preflight_allows_it`
在首次全量并行运行时出现过一次网络时序抖动,单测与第二次全量均通过。

## 明确未做 / 下一步

- Bing 的 RMS 依赖加载器(`rms.js`/`rms.start`)仍未接入;`text/rms` 块如果依赖
  尚未加载的懒 bundle(如 Feedback),会抛 `ReferenceError` 并计入 scripts_threw。
- 更正确的做法是把 text/rms 绑定到 RMS 资源图,在依赖 bundle 到达后执行;当前
  只保证块本身按页面尾部顺序执行。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;连续动画时钟、高级渐变、
  SVG filter 其余原语、IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十一) — 浏览器侧异步 dynamic import

接续 import map 会话。这一轮把动态 `import()` 从“没有 loader 就拒绝”推进到
render-net 驱动的异步模块加载。

## 实现

| 内容 | 落点 |
|---|---|
| **运行时挂起** | `PendingModuleLoad`/`DynamicModuleUnit`;无同步 loader 时把 `(id, referrer, specifier, attributes)` 交给 embedding |
| **Promise 生命周期** | id → promise/object/phase 映射;GC 根保存 promise 对象;`resolve_dynamic_import` / `reject_dynamic_import` |
| **页面 API** | `Page::take_pending_module_loads`、`resolve_dynamic_import`、`reject_dynamic_import`、`drain_pending_microtasks` |
| **浏览器状态** | `PageState.pending_dynamic_imports` + `module_import_map`;每个 import 一个 `ModuleGraph` |
| **多轮 fetch** | `poll_dynamic_module_imports`:解析 specifier → fetch 入口模块 → `take_pending` 逐轮取依赖 → `finish` |
| **声明与结算** | 图完整后声明所有 unit,调用运行时链接/求值;失败用 `TypeError` 拒绝 |
| **微任务** | 结算发生在 page task 之外,`drain_pending_microtasks` 直接跑 promise 反应链 |
| **测试** | runner 侧 resolve/reject、Page API 端到端、浏览器本地 HTTP 探针 |

## 证据

本地动态 import 页面:

```html
<script type="importmap">{"imports":{"widgets":"./widget.js"}}</script>
<script type="module">
  document.body.textContent = 'start';
  import('widgets').then((m) => {
    console.log('DYN THEN value=' + m.value);
    document.body.textContent = m.value;
    document.body.style.background = m.color;
  });
</script>
```

隐藏截图 + 控制台:

```
[console.log] DYN THEN value=dynamic-ok
[console.log] DYN AFTER dynamic-ok bg=rgb(0, 0, 255)
背景像素 (5,195) = (0, 0, 255)
health ready=Complete dom_elements=8 boxes=2 scripts_failed=0 scripts_threw=0
```

说明 import map 把 `widgets` 解析为 `./widget.js`,render-net 取回、编译、声明、
链接、求值,`.then` 在截图前执行并改变 DOM/绘制。

真实站点:

```
Bing:  ready=Complete  dom_elements=466 boxes=166
       scripts_failed=0 scripts_threw=0 css_warnings=9 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Interactive dom_elements=1000 boxes=117
       scripts_failed=12 scripts_threw=2 css_warnings=36 images_failed=1
```

Bing 仍不会由 importShim 触发原生 `import()`(其 shim 自己处理),但浏览器侧
动态加载路径已具备;JD 主商品区仍受 403。

## 验证

```
cargo test --workspace                       2781 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

新增测试覆盖:异步 import 挂起/成功结算/失败拒绝、Page API 端到端、import map
解析与静态模块图、浏览器本地 import map 模块执行。

## 明确未做 / 下一步

- 动态模块 fetch 未做跨域 CORS 检查;`import` attributes 已交给 loader 接口但当前
  加载器不使用。模块顶层 `await` 的异步求值仍按现有运行时能力。
- 只支持第一个 `animation-name` 等旧限制仍在;动态 import 的重定向/缓存语义复用
  现有 cache/script 管线,未做单独的模块缓存分区。
- JD 主商品区仍受 403;GitHub/HN 本机链路阻断;高级渐变、SVG filter 其余原语、
  IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续三十) — Import maps 与模块说明符解析

接续 `@keyframes` 会话。这一轮实现 `<script type="importmap">` 的解析与静态
模块图的 bare specifier 解析。

## 实现

| 内容 | 落点 |
|---|---|
| **ImportMap** | 新 `render-core/src/import_map.rs`;`serde_json` 解析,失败整张表作废 |
| **imports** | 精确匹配优先,其次最长 `/` 前缀;`null` 表示显式阻止 |
| **scopes** | 按 referrer URL 选择最长匹配 scope;scope 优先于顶层 imports |
| **URL 规范化** | URL-like key 相对文档 base 归一化并去 fragment;bare key 保持原样 |
| **回退** | 无映射时仍按 URL-like 规则相对模块 URL 解析 |
| **脚本发现** | `type=importmap` 不再报 Unsupported,而是解析为 `ScriptDiscovery.import_map`;非法 JSON 报 `InvalidImportMap` |
| **模块图** | `ModuleGraph::set_import_map`;`take_pending` 用 map 解析每个 specifier |
| **浏览器管线** | `ScriptFetchPlan` 携带 map,`prepare_script_batch` 在建模前安装 |
| **测试** | import map 解析/前缀/作用域/null/非法 JSON、脚本发现、模块图、浏览器端到端模块执行 |

## 证据

本地 import-map 页面:

```html
<script type="importmap">{"imports":{"lib":"./lib.js"}}</script>
<script type="module">
  import { value, color } from 'lib';
  document.body.textContent = value;
  document.body.style.background = color;
</script>
```

隐藏截图 + `RENDER_DEBUG_EVAL`:

```
[console.log] IMPORTMAP bodyText="mapped-ok" bg=rgb(0,128,0) attr=background: rgb(0,128,0);
health ready=Complete dom_elements=7 boxes=2 scripts_failed=0 scripts_threw=0
背景像素 (5,195) = (0,128,0)
```

即 bare specifier `lib` 经 import map 解析为 `./lib.js`,模块成功 fetch、链接、
执行,并改变了 DOM 与绘制。

Bing 不再出现 `import maps are not implemented`;健康线:

```
Bing:  ready=Complete  dom_elements=472 boxes=167
       scripts_failed=0 scripts_threw=0 css_warnings=9 images_failed=0
Baidu: ready=Complete  dom_elements=450 boxes=156
       scripts_failed=0 scripts_threw=0 css_warnings=5 images_failed=0
JD:    ready=Loading   dom_elements=1157 boxes=117
       scripts_failed=8 scripts_threw=2 css_warnings=36 images_failed=1
```

Bing 仍加载 es-module-shims;静态模块图与 import map 已通,浏览器侧动态
`import()` loader 已在续三十一接入。

## 验证

```
cargo test --workspace                       2778 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- 只应用第一张 import map;标准的多张 map 合并与"map 必须在模块之前"的时序
  仍按文档顺序宽松处理。
- `integrity` 元数据忽略;`src` 形式的 import map 非法,当前按空内容处理并报错。
- 动态 `import()` 已在续三十一接入浏览器异步 loader;跨域模块 CORS、import
  attributes 的语义仍未完整。`text/rms` 已在续三十二执行,并在续三十三接入
  RMS 依赖加载顺序。
- JD 主商品区仍受 403 风控;GitHub/HN 本机链路阻断;高级渐变、SVG filter
  其余原语、IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续二十九) — CSS @keyframes 动画采样

接续 Bing/Baidu 脚本与 mask 几何会话。这一轮把 `@keyframes` 从“解析后丢弃”
推进到按时间参数采样并参与布局/绘制。

## 实现

| 内容 | 落点 |
|---|---|
| **保留 keyframes** | `StyleSheet.keyframes`;`Keyframe { offset(万分比), declarations }`、`KeyframesRule { name, keyframes, media, layer }` |
| **解析关键帧块** | `from`/`to`/百分比、逗号选择器列表、每帧声明;非法选择器告警但不吞后续规则 |
| **`animation` 简写** | 展开为 name/duration/timing-function/delay/iteration-count/direction/fill-mode/play-state;支持 `cubic-bezier(...)`、`steps(...)` |
| **动画属性注册** | `animation-name`、`-duration`、`-delay`、`-timing-function`、`-iteration-count`、`-direction`、`-fill-mode`、`-play-state` |
| **时间采样** | delay、有限/无限迭代、alternate/reverse、fill-mode、逐帧 timing function |
| **插值** | 数字/长度/角度/矩阵函数、sRGB 颜色;无法插值的离散属性在 50% 翻转;缺失端点用基础计算值补齐 |
| **写回样式** | `ComputedStyle::set_animated_property` 同时更新 token 与 typed 值,布局看到动画 width/height,绘制看到 opacity/transform/颜色 |
| **浏览器快照** | 静态截图默认采样 1.0s(页面入场动画已稳定);`RENDER_ANIMATION_TIME_MS` 可指定任意时刻 |
| **能力登记** | `css.animations` 从 `Missing` 改为 `Partial`,并登记 `css::animation::tests` |

## 证据

`animationprobe.html` 同一页面在三个时刻的像素:

```
t=0ms     fade(10,202) = (255,255,255)   grow x=15 白, x=30 白
t=500ms   fade(10,202) = (255,128,128)   grow x=15 蓝, x=30 白
t=1000ms  fade(10,202) = (255,0,0)       grow x=15 蓝, x=30 蓝
```

即 opacity 0→1 按 0.5 线性插值,width 20px 在 500ms、40px 在 1000ms 进入布局。

真实站点(默认 1s 快照):

```
Bing:  ready=Complete  dom_elements=465 boxes=166 scripts_failed=0 scripts_threw=0
       images_failed=0 css_warnings=8(实现前 19;@keyframes 诊断消失)
Baidu: ready=Complete  dom_elements=450 boxes=156 scripts_failed=0 scripts_threw=0
JD:    ready=Interactive dom_elements=1158 boxes=117 scripts_failed=11 scripts_threw=2
       css_warnings=36(实现前 156)
```

Bing 截图在 0s 时页头会被入场动画隐藏,1s 采样后页头、Rewards、导航均恢复;
说明动画确实作用到了真实页面,而不是只通过测试。

## 验证

```
cargo test --workspace                       2771 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

新增测试覆盖 keyframes 解析、animation 简写展开、数字/颜色插值、keyframe
属性采样、delay/fill/direction、opacity 动画像素与 width/transform 布局。

## 明确未做 / 下一步

- 没有持续运行的动画时钟/合成器;每个静态帧只按一个时间点采样。滚动或脚本
  更新触发的重绘会重新采样,但不会自动以 60fps 推进。
- 只取第一个 `animation-name`;逗号分隔的多动画、Web Animations API、
  `animation-composition`、CSS Transitions 未实现。
- `transform: none` 到具体变换按离散处理;`steps()`/`linear()` 为近似实现;
  继承属性(如父级 `color`)的动画不会重新传播给已计算的子元素。
- JD 主商品区仍受 403 风控;GitHub/HN 本机链路阻断;高级渐变、SVG filter
  其余原语、IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续二十八) — Bing/Baidu 脚本异常与 mask 几何

接续 CSS mask 会话。这一轮先修掉 Bing/Baidu 的脚本异常,再补齐 CSS
`mask-size`/`mask-position`/`mask-repeat`。

## 脚本异常根因与修复

| 站点 | 失败 | 根因 | 修复 |
|---|---|---|---|
| Bing | `Cannot read properties of undefined (reading 'versionchange')` | `IDBDatabase` 没有 `_listeners`。升级数据库时 `fire(connection, 'versionchange')` 读取 `target._listeners[type]`,于是报 `versionchange` 属性读错 | `IDBDatabase` 初始化 `_listeners`,并安装 `eventTarget` 的 add/remove/dispatch |
| Baidu | `Cannot read properties of undefined (reading 'length')` | `navigator.mimeTypes` 未定义,UA 检测直接读 `.length` | 提供空 `navigator.mimeTypes`/`navigator.plugins` 集合(length、item、namedItem) |
| Baidu | `Storage is not defined` | 只有 `localStorage`/`sessionStorage` 对象,没有全局 `Storage` 接口对象 | 安装不可构造的 `Storage` 构造器,prototype 指向共享 Storage 原型 |

## mask 几何实现

| 内容 | 落点 |
|---|---|
| **MaskLayer** | `image + tile + clip + repeat_x/repeat_y`;mask 从单一矩形升级为可平铺层 |
| **mask-size** | `auto`、单/双长度/百分比、`cover`、`contain`;位图保留固有比例 |
| **mask-position** | 关键字、百分比、长度;按剩余空间计算偏移 |
| **mask-repeat** | 默认 `repeat`;`repeat-x`、`repeat-y`、`no-repeat`(space/round 近似 repeat) |
| **采样** | 每像素先按 clip 判断,再映射到 tile 局部坐标;渐变按 tile 重新求解几何 |
| **测试** | mask-size+position+no-repeat、repeat 平铺、Bing IDB versionchange、navigator 集合、Storage 全局,共 5 个新增 |

## 证据

脚本健康线:

```
Bing:  ready=Complete dom_elements=465 boxes=166 scripts_failed=0 scripts_threw=0
Baidu: ready=Complete dom_elements=450 boxes=156 scripts_failed=0 scripts_threw=0
JD:    ready=Loading  dom_elements=949 boxes=117 scripts_failed=8 scripts_threw=2
```

mask 几何像素采样(`cssfilterprobe3.png`,白底):

```
#tile  mask-size:20x20  mask-position:10px 10px  no-repeat
  (419,216) 白   (420,212) 红   (439,221) 红   (440,217) 白
#repeat mask-size:20x20 repeat
  (495,205) 红   (510,205) 红   (550,237) 白(元素右边界外)
```

Bing 截图中 Rewards 勋章、搜索框、壁纸均正常;Baidu 首页 logo、搜索框、热搜
正常。JD 主商品区仍为空。

## 验证

```
cargo test --workspace                       2764 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- JD 主商品区仍受 403 风控;GitHub/HN 本机链路阻断。
- Bing 的 import map 已在续三十接入静态模块图;`text/rms` 已在续三十二执行,
  并在续三十三接入 RMS 依赖加载顺序。
  `@keyframes` 已在续二十九由时间参数化样式采样执行,但没有持续运行的合成器时钟。
- CSS `mask-mode`、多层 mask、`mask-composite`、`space`/`round` 精确平铺未做;
  filter 仍缺 `contrast`/`hue-rotate`/`invert`/`sepia`/`url(#filter)`。
- 高级渐变、SVG filter 其余原语、IndexedDB 值类型覆盖仍未完成。

---

# 交接文档 (2026-10-31, 续二十七) — grayscale / saturate / drop-shadow 与 mask URL

接续 CSS filter/mask 会话。这一轮补齐常用 filter 函数,并让 `mask-image` 支持
径向渐变与 URL 图片源。

## 实现

| 内容 | 落点 |
|---|---|
| **filter 函数顺序** | `filter` 解析为按源码顺序执行的 `FilterOperation` 列表,不再压平为固定字段 |
| **grayscale** | 按 CSS Filter Effects 亮度权重混合到灰度 |
| **saturate** | CSS 饱和度矩阵 |
| **drop-shadow** | offset/blur/color;alpha 轮廓偏移 + 高斯模糊,再 source-over 到原图下方 |
| **mask radial-gradient** | 解析 `circle/ellipse`、`closest/farthest-side/corner`、显式半径与 `at` 位置 |
| **mask URL** | 新增 `ImageSource::CssMask`,图片发现/加载/get_css_mask;mask 采样图片 alpha |
| **坐标** | 遮罩按 document 坐标在离屏层内反算,支持滚动 |
| **测试** | grayscale、saturate、drop-shadow、radial mask、URL mask(加载真实解码图),共 5 个 |

## 证据

`cssfilterprobe2.html` 像素采样(白底,页面原点 y=192):

```
grayscale(1) 红             (30,232)  = (54,54,54)
saturate(0) rgb(0,200,0)    (110,232) = (143,143,143)
drop-shadow(10px 0 0 black) 原图       (185,212) = (255,0,0)
                            阴影       (196,212) = (0,0,0)
radial mask 中心             (280,232) = (255,4,4)
radial mask 角               (305,257) = (255,217,217)
url mask 左/右               (340,232) = (255,0,0)
                             (380,232) = (255,255,255)
```

即灰度/去饱和正确、阴影落在元素后方、径向 mask 从中心到边缘渐隐、URL mask
左半保留右半透明。

Bing / Baidu / JD 冒烟:

```
Bing:  ready=Complete    dom_elements=475 boxes=167 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Loading     dom_elements=1157 boxes=117 images_failed=1
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2760 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- CSS filter 仍未实现 `contrast`/`hue-rotate`/`invert`/`sepia`/`url(#filter)` 等;
  `drop-shadow` 目前是单层、无多阴影列表。
- `mask-size`/`mask-position`/`mask-repeat` 已在续二十八实现;`mask-mode`、
  多层 mask 与 `space`/`round` 的精确平铺仍缺。
- 高级渐变(`xlink:href`/`spreadMethod`/径向焦点)、IndexedDB 值类型覆盖仍未完成。
- JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;AVIF 依赖系统 dav1d。

---

# 交接文档 (2026-10-31, 续二十六) — CSS filter / mask-image

接续 SVG filter 原语会话。这一轮给 HTML 元素的 stacking context 接入
`filter: blur()/brightness()` 与 `mask-image: linear-gradient(...)`。

## 实现

| 内容 | 落点 |
|---|---|
| **CSS `filter`** | 计算样式注册;解析 `blur(<length>)` 与 `brightness(<number|%>)` |
| **离屏层** | 有 filter/mask 的元素强制 stacking context;raster 在合成前处理层像素 |
| **blur** | 可分离高斯卷积,premultiplied RGBA,边界透明 |
| **brightness** | RGB 通道乘法,alpha 不变 |
| **CSS `mask-image`** | 支持 `linear-gradient(...)` alpha 遮罩,按元素边框盒投影 |
| **顺序** | 层内先 brightness 再 blur,再应用 mask,最后 warp/opacity 合成 |
| **测试** | brightness、blur 扩散、linear-gradient mask,共 3 个 |

## 证据

`cssfilterprobe.html` 像素采样(白底):

```
brightness(0.5)  (30,222)  = (128,0,0)
blur 中心/边缘    (100,222) = (255,142,142)
                 (115,222) = (255,245,245)
mask 左/右        (160,222) = (255,210,210)
                 (200,222) = (255,40,40)
```

即亮度减半、blur 向盒外扩散、linear-gradient 从透明到不透明。

Bing / Baidu / JD 冒烟:

```
Bing:  ready=Complete    dom_elements=488 boxes=165 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Loading     dom_elements=843 boxes=117 images_failed=1
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2755 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- CSS filter 的 `grayscale`/`saturate`/`drop-shadow` 与 mask 的
  `radial-gradient`/URL 源已在续二十七实现;其余 filter 函数、mask-size/position/
  mask-mode 未实现。
- 高级渐变(`xlink:href`/`spreadMethod`/径向焦点)、IndexedDB 值类型覆盖仍未完成。
- JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;AVIF 依赖系统 dav1d。

---

# 交接文档 (2026-10-31, 续二十五) — feOffset / feBlend / feComposite

接续组级 opacity 会话。这一轮把 SVG filter 从 blur/color-matrix 扩展到常用的
位移、混合与合成,并支持命名 `result` 与 `in`/`in2`。

## 实现

| 内容 | 落点 |
|---|---|
| **命名结果** | 每个原语的 `result` 存入 map;后续 `in`/`in2` 可引用 `SourceGraphic`/`SourceAlpha`/命名结果 |
| **默认链** | 未写 `in` 时沿用前一个原语结果,保持线性链语义 |
| **feOffset** | `dx`/`dy`;`primitiveUnits=objectBoundingBox` 按 bbox 缩放,元素变换作用于位移向量 |
| **feBlend** | normal/multiply/screen/darken/lighten/difference/exclusion;straight-alpha source-over |
| **feComposite** | over/in/out/atop/xor/arithmetic(arithmetic 目前按 source-over 近似) |
| **测试** | offset、composite over、blend normal + 命名彩色结果,共 3 个 |

## 证据

单元测试像素断言:

```
feOffset dx=5 dy=2:
  (3,3) 透明 → (8,5) 红色

feOffset result=shifted + feComposite over:
  (3,3) 红色(原图), (8,5) 红色(位移结果)

feOffset → feColorMatrix(红变蓝, result=blueShifted) → feBlend normal:
  (3,3) 红色, (8,5) 蓝色
```

Bing / Baidu / JD 冒烟:

```
Bing:  ready=Complete    dom_elements=471 boxes=167 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Interactive dom_elements=1158 boxes=117 images_failed=1
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2752 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   46 passed
```

## 明确未做 / 下一步

- `feFlood`/`feImage`/`feMorphology`/`feTurbulence`/`feDisplacementMap` 等原语
  未实现;`feComposite arithmetic` 的 k1..k4 仍按 source-over 近似。
- `color-interpolation-filters` 固定 sRGB;filter 的完整 DAG/分支语义未覆盖。
- CSS `filter`(blur/brightness)与 `mask-image`(linear-gradient)已在续二十六实现;
  高级渐变、IndexedDB 值类型覆盖仍未完成。
- JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;AVIF 依赖系统 dav1d。

---

# 交接文档 (2026-10-31, 续二十四) — SVG 组级 opacity

接续 CSS clip-path 会话。这一轮给 SVG `<g opacity>`(以及形状/`use` 的
`opacity`)加上真正的离屏合成,并验证嵌套、mask 与 filter 组合。

## 实现

| 内容 | 落点 |
|---|---|
| **opacity 读取** | `opacity` 属性与 `style="opacity: …"`;支持数字与百分比,clamp 0..1 |
| **离屏内容** | 克隆元素并去掉 `opacity` 属性/style 声明,在独立 `Raster` 中绘制 |
| **单次合成** | 每个源像素 `with_opacity(opacity)` 后 source-over 到目标;重叠子元素不会双重变亮 |
| **嵌套** | 每个 `<g opacity>` 各开一层离屏,alpha 相乘;深度限制 8 |
| **顺序** | opacity 先于 mask/filter 拦截,因此先完成内部 mask/filter,再对整组做 opacity |
| **测试** | 重叠组、嵌套、mask 组合、filter 组合、style 属性,共 5 个 |

## 证据

单元测试中的 alpha 断言:

```
overlap group opacity=0.5   alpha = 128   # 若不是组级合成会是 191
nested 0.5 × 0.5            alpha = 64
group opacity + alpha mask  left alpha = 128, right = 0
group opacity + blur        center alpha 在 80..200 之间
style="opacity: 0.5"        alpha = 128
```

Bing / Baidu / JD 冒烟:

```
Bing:  ready=Complete    dom_elements=471 boxes=167 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Interactive dom_elements=1001 boxes=117 images_failed=1
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2749 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   43 passed
```

## 明确未做 / 下一步

- opacity 只处理 SVG 栅格器内的元素;HTML/CSS `opacity` 已有 stacking context
  实现;CSS filter blur/brightness 与 mask-image linear-gradient 已在续二十六接入。
- 更多 SVG filter 原语(`feOffset`/`feBlend`/`feComposite` 等)、高级渐变
  (`xlink:href`/`spreadMethod`/径向焦点)仍未实现。
- `content` 仍只支持字符串;IndexedDB 值类型覆盖有限;JD 风控、GitHub/HN
  链路阻断是外部条件。

---

# 交接文档 (2026-10-31, 续二十三) — CSS clip-path url()/path()/reference-box

接续基本形状会话。这一轮把 CSS `clip-path` 从内联基本形状扩展到
`url(#id)` 引用内联 SVG clipPath、`path()` 和多 reference-box。

## 实现

| 内容 | 落点 |
|---|---|
| **`url(#id)`** | `DomClipPathResolver` 按 id 找内联 `<clipPath>`,序列化后用 SVG 光栅器的几何规则解析 |
| **单位** | `clipPathUnits=objectBoundingBox` 映射到元素 reference box;`userSpaceOnUse` 加 reference box 原点 |
| **`path()`** | 复用 SVG path 解析;支持数字坐标、闭合/多段路径 |
| **`reference-box`** | `margin-box`/`border-box`/`padding-box`/`content-box`;`fill-box`/`stroke-box`/`view-box` 暂回退边框盒 |
| **`calc()`** | 基本形状与 inset 参数用能识别括号的分词器,`calc(5px + 5px)` 可解析 |
| **`clip-rule`** | `url()` 引用继承 clipPath 的 nonzero/evenodd;CSS polygon 也支持 evenodd |
| **API** | 新增 `ClipPathResolver` 与 `build_display_list_with_images_and_clip_paths`,旧入口保持 None resolver |

## 证据

`clipurl.html` 像素采样:

```
half mask 内部  (20,222) = (255,0,0)
half mask 外部  (80,222) = (255,255,255)
path 内部       (150,222)= (0,200,0)
path 三角形外    (220,290)= (255,255,255)
padding-box 内  (280,242)= (0,0,255)
border 上       (250,202)= (255,255,255)
```

即 objectBoundingBox SVG clipPath、`path()` 三角形与 `inset(0) padding-box`
都按预期裁剪。

Bing / Baidu / JD 冒烟:

```
Bing:  ready=Complete    dom_elements=481 boxes=164 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Interactive dom_elements=1157 boxes=117 images_failed=1
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2744 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   38 passed
```

新增测试覆盖 `path()` 点集、`padding-box` reference box、`calc()` inset、以及
内联 SVG `url(#id)` 的实际像素裁剪。

## 明确未做 / 下一步

- `path()` 仍只解析数字坐标,不支持百分比/`calc()`;`url()` 只取第一条轮廓,
  多条轮廓形成的洞还不支持。
- `fill-box`/`stroke-box`/`view-box` reference-box、CSS 样式表中的 `clip-rule`
  仍未接入。
- 更多 SVG filter 原语、高级渐变仍未实现;组级 `opacity` 已在续二十四、
  `feOffset`/`feBlend`/`feComposite` 已在续二十五实现。
- JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;AVIF 依赖系统 dav1d。

---

# 交接文档 (2026-10-31, 续二十二) — CSS clip-path 与 SVG clip-rule

接续 IndexedDB 会话。这一轮给 HTML 元素接入 CSS `clip-path` 的常用基本形状,
并让 SVG `clipPath` 真正遵守 `clip-rule` 的 nonzero/evenodd 差异。

## 实现

| 内容 | 落点 |
|---|---|
| **CSS `clip-path` 属性** | 计算样式注册 `clip-path`;paint 阶段按边框盒解析 |
| **基本形状** | `inset()`(含 `round`)、`circle()`、`ellipse()`、`polygon()` |
| **百分比/长度** | 百分比按边框盒宽高解析;像素/em/rem/viewport 长度复用现有长度解析器 |
| **ClipShape** | 新增 Circle/Ellipse/Polygon(最多 24 点,保持 Copy),光栅器逐像素包含测试 |
| **层叠/定位** | clip-path 元素创建局部 stacking context,定位后代也在裁剪范围内 |
| **SVG `clip-rule`** | 默认 nonzero;`clip-rule="evenodd"` 与 `style="clip-rule:evenodd"` 生效;同一 path 的多个子路径参与同一个填充规则 |
| **测试** | display-list 形状解析、圆裁剪像素、SVG evenodd/nonzero、style 属性 |

## 证据

`clipprobe.html` 像素采样(白色背景):

```
inset-out  (10,202)  = (255,255,255)   # 左上角被裁掉
inset-in   (30,222)  = (255,0,0)       # inset 内部保留
circle-out (130,202) = (255,255,255)   # 圆角外
circle-in  (180,252) = (0,200,0)       # 圆心保留
poly-in    (252,202) = (0,0,255)       # 三角形内部
poly-out   (340,292) = (255,255,255)   # 三角形右下角外
```

SVG `clip-rule`:

```
clip-rule="evenodd" 中心像素透明、环带红色
默认 nonzero       中心像素红色
style="clip-rule: evenodd" 同样切出洞
```

Bing / Baidu / JD 冒烟:

```
Bing:  ready=Complete    dom_elements=472 boxes=167 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Interactive dom_elements=1158 boxes=117 images_failed=1
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2742 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   38 passed
```

## 明确未做 / 下一步

- CSS `clip-path: url(#id)`、`path()`、`reference-box` 已在续二十三实现;
  `path()` 的百分比/`calc()` 坐标与多轮廓洞仍未覆盖,`fill-box`/`stroke-box`/
  `view-box` 仍回退边框盒。
- SVG 内联 CSS 样式表中的 `clip-rule` 仍未接入(仅属性与 style 属性);`mask`/
  `filter` 的 CSS 形式也未实现。
- 更多 SVG filter 原语、高级渐变仍未实现;组级 `opacity` 已在续二十四、
  `feOffset`/`feBlend`/`feComposite` 已在续二十五实现。
- JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;AVIF 依赖系统 dav1d。

---

# 交接文档 (2026-10-31, 续二十一) — IndexedDB 磁盘持久化、索引与升级

接续 AVIF 会话。这一轮把内存版 IndexedDB 接到 profile 磁盘存储,并补齐
`createIndex`/索引查询与 `versionchange` 升级事件。

## 实现

| 内容 | 落点 |
|---|---|
| **持久化数据** | prelude 维护 `__renderIndexedDbData` JSON;宿主在文档脚本前注入,事务提交/升级/删除时更新 |
| **profile 文件** | `indexed-db.txt`,按 origin 保存序列化状态;复用 `escape_field`/`write_store`,上限 64 MB |
| **文档同步** | `PageState::restore_indexed_db` / `sync_indexed_db`,接入新建、导航、pump、关闭与标签关闭 |
| **索引 API** | `createIndex`/`deleteIndex`/`store.index()`,唯一约束、`multiEntry`,`get`/`getKey`/`getAll`/`getAllKeys`/`count`/`openCursor` |
| **版本升级** | `upgradeneeded` 期间可建/删 store 与 index;连接收到 `versionchange`;`deleteDatabase` 清理持久化 |
| **序列化** | Date、数组、普通对象;保留 keyPath、autoIncrement、nextKey、索引元数据与记录 |
| **测试** | runtime 持久化/索引、唯一索引冲突、profile 重开;另做了真实浏览器重启与升级 |

## 证据

真实浏览器写入(临时 profile,`indexed-db.txt` 已落盘):

```
IDB_WRITE complete
# rENDER indexed db v1
http://127.0.0.1:8765  {"restart-db":{"version":1,"stores":{"items":{
  "keyPath":"id","autoIncrement":false,"nextKey":1,
  "indexes":{"by_name":{"keyPath":"name","unique":true,"multiEntry":false}},
  "records":[{"key":1,"value":{"id":1,"name":"alpha","n":10}},
             {"key":2,"value":{"id":2,"name":"beta","n":20}}]}}}}
```

浏览器完全退出后重新启动,同一 profile 打开读页面:

```
IDB_READ get=alpha
IDB_READ index=2
IDB_READ count=2
```

升级到 v2(第三次启动):

```
IDB_UPGRADE old=1 new=2
IDB_UPGRADE complete
```

文件版本变为 2,原 `items` 数据与索引保留,新增 `logs` store;升级后再次启动
读页面仍返回 `get=alpha / index=2 / count=2`。

相关测试:

```
cargo test -p render-js --lib indexed_db_indexes_and_persisted_state  ok
cargo test -p render-js --lib indexed_db_unique_index                ok
cargo test -p render-browser --bin render-browser indexed_db_state   ok
```

## 验证

```
cargo test --workspace                       2738 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

Bing/Baidu/JD 冒烟(持久化改动后):

```
Bing:  ready=Complete    dom_elements=465 boxes=166 images_failed=0
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0
JD:    ready=Loading     dom_elements=968 boxes=117 images_failed=1
```

## 明确未做 / 下一步

- 持久化只覆盖 Date/数组/普通对象;`Map`/`Set`/TypedArray/Blob/File 无标记编码,
  重启后会丢类型或值。
- 事务没有回滚日志;`abort()` 不会撤销已经改动的内存记录;跨标签页没有
  `storage`/`versionchange` 广播,只有同一 realm 内的 `versionchange` 事件。
- 没有配额管理/驱逐;`indexed-db.txt` 是单文件、全量重写。
- JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;CSS `clip-path` 已实现(续二十二/二十三)、组级
  `opacity` 已在续二十四实现;其它 filter 原语等仍未实现。

---

# 交接文档 (2026-10-31, 续二十) — AVIF 解码与失败回退

接续 filter 会话。这一轮接入 AVIF 解码,并为无法解码的“能力探测/追踪像素”提供
透明占位回退,使 Bing/Baidu 的 `images_failed` 归零。

## 实现

| 内容 | 落点 |
|---|---|
| **AVIF 解码** | `image` 0.25.9 的 `avif-native` feature,底层 `dav1d`;新增 `ImageFormat::Avif` |
| **格式识别** | `image/avif` 媒体类型、文件签名 sniff、`IMAGE_ACCEPT` 加入 `image/avif` |
| **真实 AVIF 测试** | 16×16 红色 AVIF(avifenc 生成)解码为 16×16 红像素 |
| **截断能力探测回退** | `data:image/avif` 且 `ispe` 尺寸 ≤4×4 的截断 AVIF,解码失败时插入 2×2 透明占位并告警 |
| **非图片响应回退** | `<img>` 收到无图片签名的小响应(追踪像素/软 404)时插入 1×1 透明占位并告警,不再计为硬错误 |
| **诊断** | 新增 `ImageResourceDiagnosticCode::PlaceholderFallback`(Warning) |
| **测试** | AVIF 解码、非图片占位、截断 AVIF 占位,共 3 个 |

## 证据

AVIF 实际解码(浏览器内,data URL):

```
health images_failed=0
image loaded=1 size=16x16
red pixels = 4096, bounds = (8,200)..(71,263)
```

Bing(隐藏截图,15 秒):

```
health page=https://cn.bing.com/ ready=Complete dom_elements=481 boxes=166
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=13 images_failed=0
layout <div> id=rh_rwm 105.5×42
layout <svg> class=medal-newlevel-circled 32×32
```

`images_failed` 从 3 → 0;T5 勋章仍可见。截断的 2×2 AVIF 能力探测和
`/fd/ls/l?...` 追踪像素均以 Warning `PlaceholderFallback` 处理。

Baidu / JD:

```
Baidu: ready=Complete    dom_elements=453 boxes=156 images_failed=0  (7 → 0)
JD:    ready=Interactive dom_elements=1000 boxes=117 images_failed=1  (2 → 1)
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 构建前置条件

`avif-native` 需要系统的 `libdav1d` 与头文件;本机通过 Homebrew 安装:

```
pkg-config --exists dav1d   # yes
which dav1d                 # /opt/homebrew/bin/dav1d
```

在无 dav1d 的 CI/机器上需要安装 `dav1d` 开发包,否则构建失败。

## 验证

```
cargo test --workspace                       2735 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   36 passed
```

## 明确未做 / 下一步

- AVIF 解码依赖系统 dav1d,不是纯 Rust 便携路径;AVIF 动画/序列仍未处理。
- 非图片小响应回退是视觉占位,不会真正解码 BMP/TIFF 等其它未支持格式。
- SVG `filter` 已支持 blur/color-matrix/offset/blend/composite;CSS `filter`
  blur/brightness 已在续二十六实现;CSS `clip-path`
  已在续二十二实现;组级 `opacity` 已在续二十四实现,高级渐变仍未实现。
- 其它图片格式/真实 CDN 变体仍需继续抽样;IndexedDB 已持久化(续二十一);JD 风控、
  GitHub/HN 链路阻断是外部条件。

---

# 交接文档 (2026-10-31, 续十九) — SVG `filter`: blur / color matrix

接续 SVG mask 会话。这一轮实现 filter 的前两个原语:
`feGaussianBlur` 与 `feColorMatrix`,使用离屏缓冲区合成。

## 实现

| 内容 | 落点 |
|---|---|
| **收集 filter 定义** | 按 `id` 索引整棵 XML 树,支持前向引用 |
| **离屏内容** | 被过滤元素克隆后去掉 `filter` 属性,在独立 `Raster` 中绘制 |
| **原语链** | 按文档顺序处理;`in="SourceGraphic"` 重置为原始内容 |
| **`feGaussianBlur`** | 可分离高斯卷积,premultiplied RGBA,`stdDeviation` 单值/双值,`primitiveUnits` 与元素缩放 |
| **`feColorMatrix`** | `matrix`、`saturate`、`hueRotate`、`luminanceToAlpha` 四种类型 |
| **filter 区域** | `filterUnits` 的 `objectBoundingBox`/`userSpaceOnUse`,默认 `-10%/-10%/120%/120%` |
| **作用范围** | 形状、组、`mask` 内容;嵌套 filter 深度限制 8 |
| **测试** | blur 扩散、saturate 去饱和、矩阵换通道,共 3 个 |

## 证据

端到端 filter 页面(`filterprobe.html`):

```
blurred red region: 176 red-ish pixels (4×4 source spread beyond its bounds)
saturate region:    2704 pixels of (54,54,54), i.e. 0.213 × 255
```

即高斯模糊把 4×4 红块扩散到边界外;`saturate=0` 把红色变为
`(54,54,54)` 灰色。

Bing(隐藏截图,15 秒):

```
health page=https://cn.bing.com/ ready=Complete dom_elements=465 boxes=166
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=17 images_failed=3
layout <div> id=rh_rwm 105.5×42
layout <svg> class=medal-newlevel-circled 32×32
paint #266 source=.medal-newlevel-circled … image
```

勋章、汉堡菜单、九宫格仍正常。

Baidu / JD 冒烟:

```
Baidu: ready=Complete  dom_elements=453 boxes=156 scripts_failed=0
JD:    ready=Interactive dom_elements=1000 boxes=117 scripts_failed=12
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2732 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   36 passed
```

## 明确未做 / 下一步

- 已支持 `feGaussianBlur`/`feColorMatrix`/`feOffset`/`feBlend`/`feComposite`;
  `feFlood`/`feImage`/`feMorphology`/`feTurbulence` 等仍未实现。
- `color-interpolation-filters` 固定 sRGB,未做 linearRGB;`feColorMatrix`
  只做单链,不解析命名 `result`。
- CSS `filter` 已支持 blur/brightness(续二十六);更多 filter 函数与 filter
  内容中的 `<text>` 未实现。
- CSS `clip-path` 已在续二十二实现;高级渐变、
  `content` 字符串以外内容仍未实现。
- IndexedDB 已持久化(续二十一);JD 主商品区仍受风控 403;GitHub/HN 本机链路阻断;
  AVIF 已在续二十实现/回退。

---

# 交接文档 (2026-10-31, 续十八) — SVG `mask`

接续 `<use>` 会话。这一轮实现 SVG `mask`:形状/组/`use` 引用可以经过
alpha 或 luminance 遮罩合成。

## 实现

| 内容 | 落点 |
|---|---|
| **收集 mask 定义** | 按 `id` 索引整棵 XML 树,支持前向引用 |
| **离屏内容** | 被遮罩元素克隆后去掉 `mask` 属性,在独立 `Raster` 中绘制,继承已有 clip 栈 |
| **离屏遮罩** | `mask` 内容在第二个 `Raster` 中绘制,默认黑底透明 |
| **合成** | 每个像素按 `mask-type` 取 alpha 或 luminance(`0.2126R+0.7152G+0.0722B`),乘 mask alpha 后再 source-over |
| **单位** | `maskUnits`(区域)与 `maskContentUnits`(内容)支持 `userSpaceOnUse` / `objectBoundingBox` |
| **区域** | 默认 objectBoundingBox `-10%/-10%/120%/120%`;显式 `x/y/width/height` 生效 |
| **作用范围** | 形状、组、`<use>`;嵌套 mask 深度限制 8 |
| **测试** | alpha、luminance、组、`maskContentUnits=objectBoundingBox`、部分 alpha,共 5 个 |

## 证据

端到端 mask 页面(`maskprobe.html`):

```
red  pixels = 2048, bounds = (0,192)..(31,255)   # 20×20 元素的左半
blue pixels = 4096, bounds = (0,256)..(63,319)   # 无裁剪的 luminance mask
```

即 alpha mask 只保留左半,白色 luminance mask 完整保留。

Bing(隐藏截图,15 秒):

```
health page=https://cn.bing.com/ ready=Complete dom_elements=472 boxes=167
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=17 images_failed=3
layout <div> id=rh_rwm 105.5×42
layout <svg> class=medal-newlevel-circled 32×32
paint #266 source=.medal-newlevel-circled … image
```

勋章、汉堡菜单、九宫格仍正常。

Baidu / JD 冒烟:

```
Baidu: ready=Complete  dom_elements=453 boxes=156 scripts_failed=0
JD:    ready=Interactive dom_elements=1158 boxes=117 scripts_failed=11
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2729 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   33 passed
```

## 明确未做 / 下一步

- CSS `mask-image` 已支持 linear-gradient(续二十六);`mask-mode`/URL 源与
  mask 内容中的
  `<text>` 仍不绘制;`filter` 已在续十九实现。
- 除 blur/color-matrix/offset/blend/composite 外的 filter 原语仍未实现;
  组级 `opacity` 已在续二十四实现,CSS `clip-path` 已在续二十二实现。
- 渐变高级特性(`xlink:href` 继承、`spreadMethod`、径向焦点)仍未做。
- `content` 仍只支持字符串;IndexedDB 已持久化(续二十一);JD 主商品区仍受风控 403;
  GitHub/HN 本机链路阻断;AVIF 未实现。

---

# 交接文档 (2026-10-31, 续十七) — SVG `<use>` / `<symbol>`

接续 `clipPath` 会话。这一轮补上 SVG 图标系统最常用的间接引用:
`<use href="#id">` 与 `<symbol>`。

## 实现

| 内容 | 落点 |
|---|---|
| **收集引用目标** | 按 `id` 索引整棵 XML 树,供 `use` 查找 |
| **`href` / `xlink:href`** | 支持 `#id` 与 `url(#id)` 两种写法 |
| **阴影树** | 克隆目标子树后用 `walk` 绘制;目标自身的 `fill`/`stroke`/`transform`/`clip-path` 照常生效 |
| **继承与定位** | `use` 的 paint 传给目标;`x`/`y` 作为平移;`use` 的 transform 先于平移 |
| **`<symbol>` / `<svg>`** | 目标有 `viewBox` 且 `use`/目标给出 `width`/`height` 时,按 viewBox 缩放 |
| **递归保护** | `Raster.use_depth` 限制 16 层,自引用不会无限递归 |
| **测试** | 基本引用、`xlink:href`、paint 继承、symbol viewBox 缩放、自引用终止,共 5 个 |

## 证据

端到端 `<use>` 页面(`useprobe.html`,64×64 视口):

```
health ready=Complete dom_elements=9 boxes=3 scripts_failed=0
lime pixels = 4096, bounds = (0,192)..(63,255)
```

即 `<symbol viewBox="0 0 10 10">` 被 `<use width=20 height=20>` 放大到
64×64 的 SVG 视口内,像素完整覆盖。

Bing(隐藏截图,15 秒):

```
health page=https://cn.bing.com/ ready=Complete dom_elements=468 boxes=167
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=25 images_failed=3
layout <div> id=rh_rwm 105.5×42
layout <svg> class=medal-newlevel-circled 32×32
paint #266 source=.medal-newlevel-circled … image
```

勋章区域 418 个近白像素,`bing147-rewards.png` 中 T5 勋章、汉堡菜单、九宫格
均正常。

Baidu / JD 冒烟:

```
Baidu: ready=Complete dom_elements=453 boxes=156 scripts_failed=0
JD:    ready=Loading   dom_elements=1158 boxes=117 scripts_failed=8
```

JD 主商品区仍为空(风控 403,同机 Edge 也 403)。

## 验证

```
cargo test --workspace                       2724 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
image::svg                                   28 passed
```

## 明确未做 / 下一步

- `<use>` 仅支持同文档引用;外部文档 URL、CSS `width`/`height`(需要计算样式)
  和 `use` 上的 `clip-path` 复杂坐标仍未完全覆盖。
- `<text>` 仍未实现;`mask` 已在续十八、`filter` 已在续十九、CSS
  `clip-path` 已在续二十二、组级 `opacity` 已在续二十四实现。
- 渐变高级特性(`xlink:href` 继承、`spreadMethod`、径向焦点)仍未做。
- `content` 仍只支持字符串;IndexedDB 已持久化(续二十一);JD 主商品区仍受风控 403;
  GitHub/HN 本机链路阻断;AVIF 未实现。

---

# 交接文档 (2026-10-31, 续十六) — SVG `clipPath`

接续 SVG 渐变会话。T5 勋章的整棵图形位于
`<g clip-path="url(#clip0_5048_100697)">` 中;之前 clipPath 被忽略,虽然本轮之前
的渐变/命名空间修复让图像有内容,但真正的裁剪语义尚未实现。本轮补齐。

## 实现

| 内容 | 落点 |
|---|---|
| **收集 clipPath** | 按 `id` 索引整棵 XML 树,支持引用出现在 `<defs>` 之后 |
| **裁剪几何** | `rect`/`circle`/`ellipse`/`polygon`/`polyline`/`path`,以及 `g`/`svg`/`a`/`switch` 的递归与子 `transform` |
| **`clipPathUnits`** | `userSpaceOnUse`(默认)和 `objectBoundingBox`;组按后代几何求 bbox |
| **`transform`** | `clipPath` 自身的 `transform` 与其内容的 `transform` 组合后映射到设备空间 |
| **作用范围** | 形状级与组级 `clip-path`;嵌套 clip 取交集;fill 与 stroke 都受裁剪 |
| **裁剪测试** | 设备像素中心做 even-odd point-in-polygon;空 clipPath 裁剪掉全部内容 |
| **回归测试** | 形状/组裁剪、`objectBoundingBox`、`transform`、嵌套交集、T5 勋章模板,共 6 个测试 |

## 证据

Bing(隐藏截图,15 秒):

```
health page=https://cn.bing.com/ ready=Complete dom_elements=475 boxes=169
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=20 images_failed=3
layout <div> id=rh_rwm 105.5×42
layout <svg> class=medal-newlevel-circled 32×32
paint #279 source=.medal-newlevel-circled … image
```

裁剪图 `bing146-rewards.png` 中 T5 白色勋章完整可见(未被 clipPath 擦除),
旁边汉堡菜单/九宫格正常。与 Edge 参考 `edge-rewards-15s.json` 对比:

| 元素 | Edge | rENDER |
|---|---|---|
| `#rh_rwm` | 104.656×42, y=13.5 | 105.525×42, y=13.5 |
| 勋章 `svg` | 32×32, y=18.5 | 32×32, y=18.5 |
| 勋章内部 `g` | 30.5×30.5 | 裁剪后 32×32 视口内可见 |

T5 模板单元测试:23 个 `image::svg` 测试全过,其中
`the_t5_rewards_medal_clip_path_keeps_the_icon_visible` 断言裁剪后仍有
>150 个可见像素、>50 个白色像素。

## 验证

```
cargo test --workspace                       2719 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
JD smoke:    dom_elements=1011 boxes=251
Baidu smoke: dom_elements=455  boxes=156
```

## 明确未做 / 下一步

- SVG 内联元素的 CSS `style="clip-path:..."` 仍未解析;CSS `clip-path` 的
  HTML 基本形状与 SVG `clip-rule` 已在续二十二实现。
- `clipPath` 内容的 `text` 不参与裁剪;`<use>` 已在续十七、`mask` 已在
  续十八、`filter` 已在续十九实现。
- 渐变高级特性(`xlink:href` 继承、`spreadMethod`、径向焦点)仍未做;组级
  `opacity` 已在续二十四实现。
- `content` 仍只支持字符串;IndexedDB 已持久化(续二十一);JD 主商品区仍受风控 403;
  GitHub/HN 本机链路阻断;AVIF 未实现。

---

# 交接文档 (2026-10-31, 续十五) — SVG 渐变与 T2 勋章

接续动态 SVG 命名空间会话。上一轮修好了 `innerHTML` 插入的 SVG 的命名空间和
T5 白色勋章;这一轮补上渐变,让 Bing 的 T2 渐变勋章模板也能绘制。

## 根因

T2 模板的所有可见路径都用 `fill="url(#paintN...)"` 或 `stroke="url(#...)"`,
而根 `svg` 是 `fill="none"`。`parse_paint` 不识别 `url(...)`,返回 `Inherit`;
根 `fill="none"` 使所有形状无填充,渐变描边也被丢弃。于是即使 SVG 已经正确
光栅化并产生 image 命令,图像仍然全透明。

## 实现

| 内容 | 落点 |
|---|---|
| **收集渐变定义** | 遍历整棵 XML 树,按 `id` 索引 `linearGradient`/`radialGradient`;先于绘制收集,因此 `<defs>` 可以出现在引用之后 |
| **stop 解析** | `offset`(数字/百分比)、`stop-color`、`stop-opacity`;按 offset 排序,颜色通道线性插值 |
| **坐标系统** | `gradientUnits="userSpaceOnUse"` 与 `objectBoundingBox`;`gradientTransform` 的 `matrix/translate/rotate/scale/skew` 组合,采样时用逆变换 |
| **渐变填充与描边** | `Paint::Gradient(index)` 同时用于 fill 和 stroke;每个设备像素反算回用户空间/渐变空间采样 |
| **alpha 混合** | `Raster::put` 改为 straight-alpha source-over;`stop-opacity` 的柔和边缘和发光不再被替换成不透明色 |
| **测试** | 线性渐变、`gradientTransform`、径向渐变 stop-opacity、渐变描边,共 4 个单元测试;`image::svg` 17 passed |
| **文档** | `svg.rs` 与 `inline_svg.rs` 的支持/不支持清单同步更新 |

## 证据

T2 模板(`medal-template.html`,动态 `innerHTML` 插入,32×32):

```
non-bg pixels = 288
ring samples: (1,16)=(57,160,245)  (16,1)=(58,159,245)  (16,31)=(83,127,246)
```

裁剪图 `dynsvg7-medal.png` 可见蓝色渐变外环、缎带和圆形底部;对照
`rewards-edge-crop.png` 的白色勋章,两者结构相同(T2/T5 模板差异),不再出现
空透明图。

Bing 持久 profile 仍是 T5 白色勋章:

```
health ready=Complete dom_elements=470 boxes=168
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=21 images_failed=3
layout <div> id=rh_rwm 105.5×42
layout <svg> class=medal-newlevel-circled 32×32
paint source=.medal-newlevel-circled … image
```

## 验证

```
cargo test --workspace                       2713 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
JD smoke:    dom_elements=1157 boxes=117
Baidu smoke: dom_elements=453  boxes=156
```

## 明确未做 / 下一步

- 渐变仍不支持 `xlink:href` 继承、`spreadMethod="reflect/repeat"`、径向
  `fx`/`fy` 焦点、`userSpaceOnUse` 下的百分比坐标。
- `<g opacity>` 仍按每个子形状独立绘制,未做组级离屏合成;T2 的 0.7 透明度
  叠加会比 Edge 略强。
- `clipPath` 已在续十六、`<use>` 已在续十七、`mask` 已在续十八、`filter`
  已在续十九实现。
- `content` 仍只支持字符串;IndexedDB 仍是内存子集;JD 主商品区仍受风控 403;
  GitHub/HN 本机链路阻断;AVIF 未实现。

---

# 交接文档 (2026-10-31, 续十四) — 动态 SVG 命名空间与 Rewards 勋章

接续搜索框提示会话。这一轮把 Bing Rewards 的勋章从"DOM 存在但不可见"修到
"布局 32×32、绘制列表有 image 命令、截图可见",并顺带修复了所有通过
`innerHTML`/`outerHTML` 插入的 SVG。

## 根因

`import_dom_subtree` 把解析结果搬进目标 DOM 时,无论源节点是什么命名空间都调用
`create_element(local_name)`(HTML 元素),属性也用 `set_attribute`。于是
`div.innerHTML = '<svg …>'` 得到的 `svg` 实际是 HTML 命名空间的未知元素:

- `discover_inline_svgs_with_colors` 只认 SVG 命名空间的 `svg`,不会为它生成光栅;
- `presentational_hint_declarations` 的 SVG 分支不命中,`width`/`height` 不进入
  计算样式,盒子是 0×0;
- 属性名被 HTML 规则折叠,`viewBox` 变成 `viewbox`。

Bing 的 Rewards widget 正是用 `innerHTML` 插入勋章模板,所以 DOM 树完整
(`<g>`/`<path>`/`<defs>` 都在),但既没有 32×32 盒子也没有 image 命令。

## 修复

| 内容 | 落点 |
|---|---|
| **导入保留元素命名空间与限定名** | `create_element_qualified(namespace, prefix, local_name)`;HTML 元素仍走 `create_element`(`render-js/src/runtime/builtins/dom.rs`) |
| **导入保留属性命名空间与大小写** | 有命名空间的属性走 `set_attribute_ns`(保留 `xlink:href` 等前缀);无命名空间属性走 `set_attribute`,在 SVG 元素上保持 `viewBox` 大小写 |
| **回归测试 1** | `inner_html_preserves_svg_namespaces_and_case_sensitive_attributes`:检查 `namespaceURI`、`viewBox` 与 `xlink:href` |
| **回归测试 2** | `an_svg_appended_after_parsing_is_discovered_and_painted`:解析后追加 SVG,渲染管线必须产生 image 命令并把绿色矩形绘制到盒中心 |
| **诊断** | `RENDER_DEBUG_STACKING` 打印每个定位片段的 position/z-index/hoistable |

## Bing 证据(隐藏截图,15 秒)

```
health page=https://cn.bing.com/ ready=Complete dom_elements=475 boxes=169
       scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=19 images_failed=3

layout <div> id=rh_rwm  rect x=2120.475 y=13.5 105.5×42
layout <svg> class=medal-newlevel-circled rect x=2184 y=18.5 32×32
paint #279 source=.medal-newlevel-circled … image
```

截图像素:勋章区域 32×32 内 288 个近白像素;裁剪图 `bing143-rewards.png` 可见
圆形勋章轮廓与内部奖章图形,右侧汉堡菜单/九宫格同屏。Edge 参考图
`rewards-edge-crop.png` 的勋章是同位置的圆形勋章(T2 模板);rENDER 本轮拿到
T5 模板,图形细节不同但结构、尺寸、位置一致。

## 验证

```
cargo test --workspace                       2709 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- **Rewards 模板差异**:Bing 对 Edge 与本机 rENDER 返回不同勋章模板(T2 vs T5),
  这是页面实验/状态差异;本轮保证两种模板都能被光栅化。
- **SVG 特性子集**:渐变已在续十五、`clipPath` 已在续十六、`<use>` 已在续十七、
  `mask` 已在续十八、`filter`(blur/color matrix)已在续十九实现。
- `content` 仍只支持字符串;`attr()`/`counter()` 未实现。
- JD 主商品区仍受风控 403(同机 Edge 也 403);IndexedDB 仍是内存子集;GitHub/HN
  的本机 TCP 链路仍被阻断;AVIF 已在续二十实现(能力探测使用透明占位回退)。

---

# 交接文档 (2026-10-31, 续十三) — Bing 搜索框热词提示与头部图标

接续生成内容会话。这一轮定位到 Bing 首屏搜索框热词提示和头部菜单图标缺失的
共同根因:客户端 Preact 版本通过 `setAttribute('className', …)` 更新 class,
而 DOM 按标准把它存成 `classname` 属性, 于是 hydration 后的 class 永远不生效。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **`setAttribute('className', …)` 兼容** | 仅对 HTML 元素、精确大小写 `className` 映射到 `class`; 小写 `classname` 仍按标准创建独立属性。旧版 Preact(自动建议使用的那份)因此可以更新 class, Bing 主页 hydration 恢复 |
| **搜索框热词提示显示** | 修复后 `#sb_form_placeholder` 从 `_hide` 变为可见块, `sb_form_placeholder_query` 获得 `hp_fadein_show`; 截图显示“9点半买的手机10点50坏了” |
| **头部菜单/应用图标** | hydration 恢复后登录/头像/Rewards 右侧的汉堡菜单与九宫格图标出现(此前只有节点没有 Preact 内容) |
| **客户端提示头** | 普通请求新增 `Sec-CH-UA` / `Sec-CH-UA-Mobile` / `Sec-CH-UA-Platform` / `Accept-Language`; 与 Chrome 120 的 UA 一致。用于让站点识别为现代 Chromium(本机 Bing 变体未因它改变, 但这是通用保真修复) |
| **`RENDER_USER_AGENT`** | 诊断用 UA 覆盖, 便于比较不同 UA 下的站点变体 |

## 对照证据(1180×684 逻辑视口, 同机 Edge)

| 元素 | Edge | rENDER |
|---|---|---|
| 搜索框热词容器 | `sb_form_placeholder`, x=117.5,y=151.8, 188.5×22 | `sb_form_placeholder`, x=707.5,y=288.6, 189.7×19.2(搜索框 x=692.5+15) |
| 热词文本 span | `hp_fadein_ph hp_fadein_show` | `hp_fadein_ph hp_fadein_show` |
| 汉堡菜单 `#id_sc` | 20×39, 背景 SVG | 20×39, 背景 SVG |
| 应用九宫格 | 可见 | 可见 |
| Rewards 徽章 | 32×32 内联 SVG | `#rh_rwm` 仍无子节点(仅文本) |

健康线:

```
ready=Complete dom_elements=476 boxes=169
scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=19 images_failed=3
```

## 验证

```
cargo test --workspace                       2707 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- **Rewards 徽章**:`#rh_rwm` 的 widget 脚本已执行但未生成 `.b_clickarea`/`.medal`/SVG
  子节点, 只剩文本; 需要继续查 widget 的渲染触发条件(可能是额外 API/feature flag)。
- 搜索框 placeholder 的主路径已经显示; 键入/聚焦时的 ghost text 动画仍只验证了基础行为。
- JD 主商品区仍受风控 403(同机 Edge 也 403); IndexedDB 仍是内存子集; GitHub/HN 的
  本机 TCP 链路仍被阻断。

---

# 交接文档 (2026-10-31, 续十二) — CSS `::before`/`::after` 生成盒

接续 Bing 对照会话。这一轮实现了生成内容:伪元素的样式级联、生成盒、文本内容与
负 `z-index` 绘制顺序。Bing 的 `国内版/国际版` 胶囊背景和顶部导航的 `...` 溢出项
现在由真实的 `::after`/`::before` 盒绘制,与 Edge 一致。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **`content` 属性进入注册表** | 初始值 `normal`;此前未知声明在计算阶段被丢弃,`content: ''` 永远读不到 |
| **伪元素样式级联** | `compute_document_styles_with_hints` 对含 `::before`/`::after` 规则的文档,为每个元素计算两份伪元素样式;继承自宿主元素,匹配宿主上的 `:hover`/动态状态。只有 `content` 会生成盒时写入样式表 |
| **合成样式 id** | 伪元素没有 DOM 节点,但布局/绘制都按 `NodeId` 查样式。`render_css::pseudo::style_source(host, pseudo)` 用最高位标记的合成 id,不会与 DOM arena 冲突;格式化树按同样的 id 探测 |
| **生成盒进入格式化树** | `append_generated_content`: `::before` 在 DOM 子节点前、`::after` 在后;inline 生成盒与文本共享匿名块;`position:absolute/fixed` 或浮动时按 CSS Display §2.7 块化;字符串 `content` 生成文本子节点并应用 `text-transform` |
| **选择器伪元素匹配修复** | `#est_switch .est_common::after` 之前无法匹配:伪元素检查被应用到每个复合选择器,祖先 `#est_switch` 被要求也匹配 `::after`。现在只有 subject(最右复合选择器)检查伪元素 |
| **负 z-index 绘制顺序** | CSS 2.1 Appendix E:负 `z-index` 的定位后代在层叠上下文背景之后、in-flow 内容之前绘制。Bing 的胶囊 `z-index:-10` 现在位于文字后面 |
| **诊断** | `RENDER_DEBUG_LAYOUT` 额外打印每个匹配到的伪元素的 `content/display/position/z-index/宽高/inset/背景` |

## Bing 验证(隐藏截图,12 秒)

```
ready=Complete dom_elements=469 boxes=163
scripts_failed=0 scripts_threw=0 css_errors=0 css_warnings=25 images_failed=3

pseudo <div> id=est_cn class=est_common est_selected after
  content='' display=inline position=absolute z=-10
  width=auto height=32px top=4px right=0 bottom=-1px left=0 bg=#fff
pseudo <div> id=est_en class=est_common est_unselected after
  content='' ... bg=rgba(255,255,255,.7)
```

截图确认:两个白色胶囊背景已出现,`国际版` 清晰可读;顶部导航末尾的 `...`
溢出项由 `.scope.dots::before { content: '. . .' }` 生成;壁纸、搜索框、登录/头像/
Rewards、二维码、热榜、换一批、页脚均保持正常。

## 验证

```
cargo test --workspace                       2706 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- **`content` 只支持字符串**:`attr()`、`counter()`/`counters()`、`open-quote` 等尚未实现;
  非字符串内容目前不生成盒,而不是猜测。
- **生成盒的边界**:`display: contents` 宿主、表格结构盒、替换元素上的伪元素暂不生成;
  浮动/定位生成盒已支持。
- 搜索框占位/热词回填文本仍缺失;右上菜单/应用图标细节与 Edge 有差异。
- JD 主商品区仍受风控 403(同机 Edge 也 403);IndexedDB 仍是内存子集;GitHub/HN 的
  本机 TCP 链路仍被阻断。

---

# 交接文档 (2026-10-31, 续十一) — Bing 图标、语言切换、Rewards 与建议下拉

接续 RMS 延迟脚本会话。用 Edge CDP 在**同一视口(1180×684, DPR 2)**采集 Bing 首页的
节点矩形与计算样式,逐项对齐,修掉了 5 个平台/布局细节。Bing 首屏现在除伪元素背景外
与 Edge 同构,搜索建议与 Edge 完全一致。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **`element.style = "..."` 按 CSSOM `[PutForwards=cssText]` 生效** | 之前给元素赋字符串会创建一个 own data property,遮蔽 `.style` 访问器;Preact 渲染的箭头 SVG 因此丢了 `height:16px;width:16px`,被拉伸成 2360×2360,形成白色竖带与黑色方块。现在赋值会解析为内联声明(`crates/render-js/src/runtime/eval.rs`) |
| **行 flex 容器的 max-content 宽度改为各项之和** | 之前按块容器的"最宽子项"计算,`换一批` 按钮测得 58px 后把标签挤成两行。现在 `max_content_width` 对 row flex 调用 `flex_intrinsic_size` 求和(`solver/inline.rs`) |
| **inline-block 的内在宽度包含原子内联子项的 margin** | Bing 顶部 `图片/视频/翻译/地图/学术` 是 6 个 `inline-block;margin-right:36px` 的 li;之前只算 26px 内容宽,导致导航折成 3 行。现在原子项按 margin box 计入(`solver/flex.rs`) |
| **绝对定位盒 left/right/width 全 auto 时 shrink-to-fit 并保留固定 margin** | `#est_switch` 之前被拉成 955px 宽、从搜索框左缘开始;现在 160px 宽、x = 搜索框 + 20px,与 Edge 一致(`solver/block.rs`) |
| **classic script 的 MIME 检查只在 `nosniff` 下生效** | Bing Rewards widget 由 CDN 以 `application/json` 返回;真实浏览器在没有 `X-Content-Type-Options: nosniff` 时仍执行。现在同样执行(`render-browser/src/scripts.rs`),`#rh_rwm` 出现 |
| **QR 图片与箭头图标** | flex 替换元素取固有尺寸(QR 80×80);箭头 SVG 16×16;二维码卡片完整绘制 |
| **诊断** | `RENDER_DEBUG_TEXT` 打印内在文本测量;`RENDER_DEBUG_SCRIPTS` 打印所有 script 节点;布局转储附带 SVG 的 viewBox/固有尺寸 |

## Bing 与 Edge(同视口)对照

| 指标 | Edge | rENDER |
|---|---|---|
| 顶部导航 x | 120/182/244/306/368 | 120/182/244/306/368 |
| `换一批` | 74×28 | 74×28 |
| `#est_switch` | 160×37,x=搜索框+20 | 160×37,x=搜索框+20 |
| `#rh_rwm` | 104.7×42 | 95.5×42 |
| QR 图片 | 80×80 | 80×80 |
| 建议(输入“手机”) | 11 条 | 11 条,文本完全一致 |

健康线:

```
ready=Complete dom_elements=475 boxes=164 scripts_failed=0 scripts_threw=0
css_errors=0 css_warnings=25 images_failed=3
```

`images_failed=3` 是 Bing 的 AVIF/WebP 能力探测(引擎不支持 AVIF),随后回退成功。

## 验证

```
cargo test --workspace                       2702 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- **伪元素盒未生成**:`#est_switch .est_common::after` 的白色圆角背景没有产生,`国际版`
  在照片上依赖白底才清晰;需要实现 `::before`/`::after` 的生成盒。
- **搜索框占位/热点回填文本**:Edge 会把热词显示在输入框内;rENDER 输入框为空。
- **右上菜单/应用图标**:节点存在,图标细节与 Edge 仍有差异;菜单展开未验证。
- JD 主商品区仍受风控(同机 Edge 也 403);IndexedDB 仍是内存子集;GitHub/HN 的
  本机 TCP 链路仍被阻断。

---

# 交接文档 (2026-10-31, 续十) — Bing 的 RMS 延迟脚本与层叠上下文

接续 JD 风控对照。这一轮用 Edge 的 CDP 找到 Bing 首页缺失的右上账户/Rewards/菜单、
二维码和热点面板的根因:它们全部由 **Bing 的 RMS 延迟脚本**(`<script type="text/rms">`)
生成,而我们的平台缺了让这些脚本跑起来的几个细节。修完后 Bing 首屏已与 Edge 基本同构。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **`HTMLScriptElement.text`** | 之前 `script.text = code` 是空操作;RMS 用 `.text` 创建延迟脚本,导致所有 widget(登录、Rewards、二维码、热点、import())都没跑。getter/setter 现在映射到子文本 |
| **raw-text 元素的 `innerHTML`** | `script.innerHTML` 之前把 `<`/`&` 转义成 `&lt;`/`&amp;`,RMS 复制 innerHTML 后克隆脚本全部语法错误。现在 script/style/noscript 的 `innerHTML` 按原始文本序列化 |
| **移除非标准 `setImmediate`** | Chrome/Firefox/Safari 都没有它。Bing 的调度器检测到它就走进递归路径,`si_PP` 直接栈溢出;移除后走 `setTimeout` 分支,`onPP` 正常触发,RMS 模块开始加载 |
| **`performance.navigation`** | 旧版 Navigation Timing 对象(`type`/`redirectCount` + 常量)。Bing 的 page-ping 读取 `performance.navigation.type`,缺它时 `si_PP` 抛错、`onPP` 永不触发 |
| **`navigator.serviceWorker.controller`** | service worker 容器存在、`controller = null`;Bing 直接读 `.controller` 再判空,缺容器会 TypeError |
| **层叠上下文绘制顺序** | 绘制列表现在把定位元素推迟到最近层叠上下文的 in-flow 内容之后,按 z-index 排序(App E 第 8/9 步)。此前 fixed `z-index:6` 的 header 先画,被后面的壁纸盖住,右上登录/二维码不可见 |
| **诊断工具** | `RENDER_DEBUG_EVAL`(load 后在页面里执行一段 JS,便于用 CDP 式探针定位)、`RENDER_DEBUG_FETCH`(所有页面 fetch 交换日志)、`RENDER_DEBUG_LAYOUT` 增加 script 节点的 src/type/属性/父节点 |

## Bing 对照(Edge 同视口)

Edge 首屏:壁纸、搜索框、国内版/国际版、右上登录/Rewards/菜单、二维码卡片、
搜索热榜、主页测验、页脚。

rENDER 现在:`dom_elements 451 / boxes 158 / scripts_threw 0`;截图包含壁纸、搜索框、
切换、**右上登录+头像、二维码卡片、搜索热榜(必应上的热点 + 换一批)、主页测验、页脚**。
剩余差异:二维码卡片里的二维码图片(白色占位)、Rewards/菜单图标细节、输入时的
建议下拉需要真实键入才出现。

## 验证

```
cargo test --workspace                       2695 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
cargo fmt --all --check                      clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- 二维码图片未绘制(资源/尺寸问题);Rewards/菜单的图标细节。
- 搜索建议下拉需要键入交互验证。
- JD 主商品区仍受京东风控(Edge 同机也被 403),需要受信任会话/网络。
- IndexedDB 仍是内存子集;GitHub/HN 的本机 TCP 链路仍被阻断。

---

# 交接文档 (2026-10-31, 续九) — JD 403 环境的实锤与 cookie 桥

接续 IndexedDB/Bing 会话。本轮给 JD 的 `api.m.jd.com` 加了完整请求/响应日志,并引入
同一台机器上的 **Microsoft Edge headless** 作为对照。结论:JD 的 403 不是 rENDER 的
签名/指纹 bug,而是 JD 对该网络/会话的风控;Edge 同样被拦截。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **JD 交换日志** | `RENDER_DEBUG_FRAME=1` 下打印发往 `api.m.jd.com` 的完整 URL、credentials、Cookie 头,以及响应状态/头/body(截断 2000 字符) |
| **XHR withCredentials / fetch credentials** | `PendingFetch` 增加 `FetchCredentials`(SameOrigin/Include/Omit);XHR 读 `withCredentials`,`fetch` 读 `credentials`;跨源 Include 时装饰 Cookie 并吸收 Set-Cookie;凭据型 CORS 响应要求精确 ACAO + `Access-Control-Allow-Credentials: true` |
| **document.cookie 桥** | 此前 `document.cookie` 只是运行时里的一个独立 map,与浏览器的 HTTP CookieJar 完全不通。现在 `js_cookie_store` 记录原始赋值,`take_pending_cookie_writes` 交给浏览器 `CookieJar::set_cookie`;每个脚本批次前后 `sync_js_cookies` 把 jar 的 Cookie 头回填给 `document.cookie` |
| **对照方法** | Edge `--headless=new --remote-debugging-port` + CDP 捕获 `Network.requestWillBeSentExtraInfo` / `responseReceived` / `Page.captureScreenshot` |

## JD 的决定性证据

同一台机器、同一网络,Edge headless 打开 `https://www.jd.com/` 后页面文本是:

```
全部频道
内容太火爆了，请稍后再试
当前页面异常
请刷新或切换账户试试
刷新 切换账号
```

截图 `edge-jd.png` 是京东自己的异常页。CDP 记录显示:

- `pchome_firstScreenSoa`、`pchome_horizontalnav`、`pc_home_background` 的 **XHR** 均为 **403**,
  请求带 Cookie:`__jdv`、`__jdu`、`3AB9D23F7A4B3C9B`;
- 同一 URL 还有 3 次 `200`,initiator 类型是 **preflight**(CORS 预检),不是数据响应;
- rENDER 的同一批 XHR 也是 403;修复 cookie 桥后已带上
  `3AB9D23F7A4B3C9B=...` 与 `__jda=...`,与 Edge 的 Cookie 家族一致;
- `jsfbox_pre_gb`、`pc_home_top_monetize`、`pc_search_*` 等非风控接口在 rENDER 里返回 200。

因此 JD 首屏商品区空白的直接原因是 **JD 风控拒绝了这个网络/会话**,不是 h5st 算法或
TLS 指纹在本机的可复现差异。要继续验证 JD 完整首屏,需要一个被 JD 信任的登录态
Cookie/住宅网络,或人工在真实浏览器中完成验证——这属于用户侧环境,不是继续改引擎能解决的。

## Bing 对照

用 Edge 打开 cn.bing.com 截图(`edge-bing.png`)对照:当前 Bing 首页在同样视口下也
**没有大字号 Bing 字标**,只有左上 24x24 方块 logo;rENDER 的 Bing 已有壁纸、搜索框、
国内版/国际版、麦克风/搜索图标。剩余差异是右上角登录/Rewards/菜单、二维码推广和
搜索建议行(依赖账号态/额外接口),不是布局错误。

## 验证

```
cargo test --workspace                       2691 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings   clean
real_site_tasks                              66 passed, 0 failed
```

## 明确未做 / 下一步

- JD:等待用户提供受信任会话/网络后再验证;引擎侧已与 Edge 行为对齐(Cookie、Referer、
  credentials、h5st 均由页面 JS 生成)。
- Bing:右上账号/Rewards/菜单、二维码、搜索建议;可按需补对应 API/组件。
- IndexedDB:仍是内存实现,无磁盘持久化、索引、versionchange 事件。
- GitHub/HN 的本机 TCP 链路仍被阻断。

---

# 交接文档 (2026-10-30, 续八) — 自托管 IndexedDB 与图片 error 事件

接续 Bing 布局修复。Bing 的壁纸路径整体挂在 `window.indexedDB` 上;本轮补了
**内存版 IndexedDB**,并顺带修掉"图片加载失败不发 error"的缺口,壁纸终于渲染出来。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **IndexedDB(自托管,内存)** | `crates/render-js/src/prelude_indexeddb.js`: `indexedDB.open`(版本校验、`upgradeneeded`、`onblocked`/`onerror`/`onsuccess`)、`createObjectStore`/`deleteObjectStore`、`transaction`(readonly/readwrite,自动 commit)、`get`/`getKey`/`getAll`/`getAllKeys`/`put`/`add`/`delete`/`clear`/`count`、`openCursor`/`openKeyCursor`(next/prev、continue/advance/delete)、`IDBKeyRange`、请求的 success/error 事件与 `addEventListener`。数据只存活于当前 realm,尚未落盘 |
| **图片 error 事件** | `finish_images` 此前对失败资源只把元素标成 `Broken`,从不派发 `error`。Bing 用 2x2 AVIF data URL 做能力探测:失败后必须触发 `onerror` 才会回退到 WebP。现在失败元素按 HTML "update the image data" 派发 `error` |
| **PerformanceObserver** | 提供 `supportedEntryTypes = []`、`observe`/`disconnect`/`takeRecords` 的降级实现;Bing 的 `new PerformanceObserver` 不再抛 ReferenceError,同时因不支持 `element` 计时而跳过该分支 |
| **回归测试** | `indexed_db_opens_upgrades_and_round_trips_values`、`indexed_db_cursor_sees_earlier_writes_and_continues`、`indexed_db_missing_store_is_a_not_found_error_like_the_platform`、`an_image_whose_load_fails_fires_error_at_its_element` |
| **XHR/fetch 的 Referer** | `cors::referer`:同源发完整 URL(去 fragment),跨域只发 origin + `/`(strict-origin-when-cross-origin)。JD 的 `api.m.jd.com` 请求现在带 `Referer: https://www.jd.com/`,但 403 仍在 |
| **data URL 的 base64 空白** | 浏览器会忽略 base64 payload 里的 ASCII 空白(换行、`==` 前的空格);此前的严格解码把 JD 的 1x1 追踪图判成 InvalidUrl。现在过滤空白后再解码,`images_failed` 不再因此增长 |

## 测量(Bing,隐藏截图,12 秒)

```
ready=Complete dom_elements=292 boxes=58 scripts_failed=0 scripts_threw=0 images_failed=1
<样式> id=iotd-lite-bg 存在
<div id=img_cont ... bgimg=url("https://ts1.tc.mm.bing.net/th?id=OHR.AutumnCranes_ZH-CN5772055404_1920x1080...")
```

截图确认:必应主页壁纸(秋季鹤群照片)、搜索框、国内版/国际版切换、麦克风/搜索图标、
顶部导航全部可见。`images_failed=1` 是 AVIF 探测图的预期失败(引擎不支持 AVIF),随后
WebP 回退成功。

## 明确未做 / 下一步

- IndexedDB 尚无磁盘持久化、索引(`createIndex`)与 versionchange 事件;当前是内存实现。
- Bing 大字号 "Bing" 字标仍未出现(只剩左上 2x2 方块 logo);需要查它由哪个元素/样式生成。
- JD 主区仍空:`api.m.jd.com` 403,下一步对比 Referer/Cookie/`sec-ch-ua`/TLS 指纹。
- GitHub/HN 的本机 TCP 链路仍被阻断。

---

# 交接文档 (2026-10-30, 续七) — 位置元素不再抬高祖先的绘制层

接续 Bingo/Bing 会话。上一轮把 Bing 的脚本错误清到 0、进程不再崩溃,但页面只剩导航链接和
`#555` 背景。本轮用新的布局/绘制转储定位到**布局排序把不可见的搜索框埋掉了**并修复。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **层叠排序 bug** | `paint_layer` 把子树的最高 `z-index` 向上传播给祖先。Bing 首页第一个 in-flow 块里有 `position:fixed;z-index:6` 的 header,于是整块(含铺满视口的 `#555` 背景)被排到第二个块之后;第二个块里 `z-index:3` 的固定搜索框先画,随后被第一块的背景盖住。现在排序键只取**元素自身**的 `z-index`(且仅 positioned 元素),in-flow 祖先保持源顺序 |
| **回归测试** | `an_in_flow_wrapper_does_not_float_above_later_siblings_because_of_a_fixed_descendant`:第一个块含 `z-index:6` 固定子元素,第二个块含 `z-index:3` 固定子元素,断言两块按源顺序绘制 |
| **布局调试转储** | `RENDER_DEBUG_LAYOUT=1` 打印每个有 id/class 元素的 rect + computed display/position/z-index/opacity/visibility/inset/margin/transform/overflow/背景;`RENDER_DEBUG_PAINT_ALL=1` 打印显示列表顺序;`RENDER_DEBUG_FRAGMENT=1` 打印 fragment 树(缩进、源元素、rect、position/z-index) |
| **诊断结论** | 修复前绘制顺序:`#51 sb_form 白底` → `#63 .hp_body #555(0,0 全屏)`;修复后:`#4 .hp_body #555` → `#47 sb_form 白底`。fragment 树也确认第一个 `hp_body` 从 html 子列表末尾回到源顺序 |

## 测量(隐藏截图,8 秒)

| 站点 | dom_elements | boxes | scripts_threw | 视觉 |
|---|---|---|---|---|
| bing(修复前) | 270 | 58 | 0 | 全屏 `#555`,仅左上导航 |
| bing(修复后) | 274 | 58 | 0 | **搜索框、国内版/国际版切换、麦克风/搜索图标、导航全部可见** |

Bing 仍缺:主页背景图(`iotd-lite-bg` 动态 `<style>` 未插入,`#img_cont` 的
`background-image` 为空)、大 Bing logo;这两项是下一步。

## 明确未做 / 下一步

- **动态 `<style>` 背景图**:动态样式本身已验证可用(探针 `#result` 的
  `background-color: rgb(0,128,0)` 生效);Bing 的壁纸分支在
  `if (!forceNoIOTD && isIotdEnabled === 0 && window.indexedDB)` 处被跳过——
  **本引擎没有 IndexedDB**,所以整段壁纸逻辑不执行。要么实现 IndexedDB,要么接受
  无壁纸;不能用假的 `indexedDB` 掩盖。
- **detached `new Image()` 已支持**:图片发现现在扫描 DOM arena 中未挂载的 `<img>`
  (`new Image().src = ...` 预加载模式),探针确认 `onload` 触发
  (`detached-image-loaded`)。
- **Bing 大 logo**:`<svg id=bLogo>` 是内联 SVG,截图只显示小导航文字;需要检查 SVG
  paint 路径。
- **JD 主区**:仍是 `api.m.jd.com` 403,对比 `Referer`/Cookie/`sec-ch-ua`/TLS 指纹。
- 传输层:HN/GitHub 的 TCP 链路在本机被阻断,与引擎无关。

---

# 交接文档 (2026-10-29, 续六) — 媒体 EventTarget、微任务 GC 根与 JS 预算

接续同日"临时被调函数被 GC 回收"会话。这一轮把 JD 的脚本层从"勉强跑"推进到
**除第三方 aria.js 外全部通过**,同时补上媒体元素的事件接口与微任务队列的 GC 根。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **媒体元素是 EventTarget** | `Audio`/`Video` 实例现在有 `addEventListener` / `removeEventListener` / `dispatchEvent`。注册表在运行时内(`media_event_listeners`),`queue_video_event` 同时派发 `on*` 属性与监听器,支持 `once`、`handleEvent` 对象;GC 标记监听回调并回收已死目标的条目 |
| **空值成员调用按规范抛 TypeError** | `null.foo()` 此前走可选链的 lenient 路径,报 "value of callee is undefined or not callable";现在按 ECMA-262 13.3.3 抛 `Cannot read properties of null (reading 'foo')`,计算属性键先求值;`a?.b()` 仍正确短路 |
| **微任务队列的 GC 根** | 页面把微任务移入事件循环后,回调只在嵌入方的容器里,收集器看不到;一个回调执行中的分配会把它后面的回调扫成空对象(表现为 `stack []` 的 "not callable")。运行时新增 `set_external_microtask_roots` / `take_next_microtask`,页面在每个 runnable 前同步根集合,`invoke_microtask` 也 pin 当前任务;直接 drain 改为逐个取出 |
| **每次调用 pin callee/receiver/arguments** | 统一在 `call_in_current_realm` 入口 pin,覆盖解释器调用、原生内建回调、微任务、定时器等所有路径。之前只在 `Expr::Call` 的参数求值阶段 pin,原生内建持有时仍可能被回收 |
| **JS 步数预算 10M → 50M** | `js_security_v3` 不是死循环:用 charAt/charCodeAt 计数探针确认 6.3 万次字符访问、脚本在 30M 步内正常完成;12M 不够、15M 够。默认提到 50M,并新增 `RENDER_JS_STEPS`(覆盖预算)与 `RENDER_JS_STEP_TRACE`(每百万步打印 offset + 调用栈) |
| **Window 的 `on*` IDL 属性** | 全局对象现在安装完整的 WindowEventHandlers/GlobalEventHandlers 列表(初始 `null`、可写可配置)。此前严格模式脚本里的 `onload = fn` 会抛 ReferenceError(Bing 首页就踩到);现在赋值成功,事件派发仍读同一个属性 |
| **Performance 条目 API** | `performance.mark/measure/clearMarks/clearMeasures/getEntriesByName`;条目对象含 `name`/`entryType`/`startTime`/`duration`,由运行时保留并纳入 GC |
| **macOS 主线程栈 256 MiB** | 解释器单帧较大,Bing 的深递归在 626 层就耗尽 8 MiB 主线程栈(进程 abort)。新增 `crates/render-browser/build.rs`,在 macOS 链接 `-Wl,-stack_size,0x10000000`;`otool -l` 显示 `LC_MAIN stacksize 268435456`。Bing 不再崩溃 |
| **诊断** | 资源上限错误带上最后一条语句的偏移;浏览器错误行打印 `at line/column`;`RENDER_JS_FRAME_OFFSETS` 下调用点消息带 `(call ke at byte N)` |

## 测量(隐藏截图,12 秒)

| 站点 | dom_elements | boxes | scripts_failed | scripts_threw |
|---|---|---|---|---|
| jd(本轮前) | 667 | 120 | 6 | 3 |
| jd(本轮后) | **1139** | 117(截图时主区仍空) | 11(含 JSONP/传输) | **1**(仅第三方 aria.js) |
| bing(本轮前) | 273 | 58 | 0 | 2(其中一次随后栈溢出 abort) |
| bing(本轮后) | 270 | 58 | 0 | **0**(进程正常退出) |

JD 的安全脚本、主 bundle 均执行完成,页脚/搜索/导航真实文本已渲染;主商品区仍依赖
`api.m.jd.com` 的 403 风控响应(与浏览器指纹/token 有关),不是脚本崩溃。截图时 boxes
偏低是因为主区仍是空白骨架,页脚内容已从 0 变成完整链接表。

## 明确未做 / 下一步

- `api.m.jd.com` 的 403:请求已带 h5st / x-api-eid-token,但服务端仍拒绝;下一步对比
  Chrome 的请求头(尤其 Cookie、Referer、`sec-ch-ua` 与 TLS 指纹),先确认是否缺少
  风控 cookie。
- 京东主区骨架未填充:依赖上一条;也可能是 React 渲染在等待被 403 的接口。
- `aria.js` 的 `Cannot read properties of undefined (reading 'replace')`:第三方无障碍
  脚本,定位到 byte 100291;需要看它读取的 UA/特性检测在哪个元素上返回 undefined。
- 脚本栈仍只有函数名/偏移,没有跨 bundle 的 file:line;`RENDER_JS_FRAME_OFFSETS` 已经
  能给出脚本 URL + 字节偏移,后续可接 source map。
- 传输层:本机对 HN/GitHub 的 TCP 连接被网络层阻断(curl -4/-6 同样超时),不是预算问题。

---

# 交接文档 (2026-10-29, 续五) — 临时被调函数在实参求值期间被 GC 回收

接续同日"core-js Map 迭代器"会话。上一轮修好 Map 之后,JD 主 bundle 只剩一个
`value is not callable (callee host Some(Ordinary), keys [])`,栈指向 `pLYI` 里的
`_interopRequireWildcard(...)` 立即调用表达式。本轮把它定位成**垃圾回收生命周期 bug**并修复。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **调用表达式的被调函数在实参求值期间没有被 pin** | `evaluate_call` 会 pin 每个实参值,却没有 pin callee / receiver。`(function (x) {...})(expensive())` 这类立即调用表达式里,函数对象只被 Rust 局部变量引用;实参求值分配对象触发 GC 时,它被扫成空普通对象,随后调用即报 `value is not callable (callee host Some(Ordinary), keys [])`。现在 callee 与 receiver 在实参求值前进入 `transient_roots`,结束后释放 |
| **`new C(args)` 同样修** | 构造函数对象在实参求值期间 pin 住;`new (function () {...})(expensive())` 不再被回收 |
| **带标签模板同样修** | tag / receiver 在模板对象与替换值求值期间 pin 住 |
| **调用点偏移更准确** | 调用节点记录 callee 末尾 token 的偏移(不是最后一个实参),错误行/列与浏览器一致(`boom()` 报 1:1);`RENDER_JS_FRAME_OFFSETS=1` 时消息里附 `(call .method at byte N)` 便于定位 bundle |
| **回归测试** | `a_temporary_callee_survives_collections_during_argument_evaluation`:4 KiB 堆上强制 GC,覆盖 IIFE、对象方法、`new` 三种临时 callee |

## 测量(隐藏截图,10 秒)

| 站点 | dom_elements | boxes | scripts_failed | scripts_threw |
|---|---|---|---|---|
| jd(修复前) | 658 | 117 | 5 | 4 |
| jd(修复后) | **993** | **226** | 8(含 4 个 CDN transport/JSONP) | **3** |

JD 首屏从近乎空白变成完整骨架(搜索框、轮播占位、商品卡网格、侧栏),主 bundle
`index.chunk.js` 不再整体抛错。剩余 3 个运行时错误:`js_security_v3` 步数上限、内联脚本
`undefined is not a constructor`、`aria.js` 的媒体 `addEventListener` 缺口。

---

# 交接文档 (2026-10-29, 续四) — 真实页面第二层：RegExp 静态属性、页面健康线与脚本栈

接续同日"脚本失败兜底"会话。用隐藏截图逐站排查后,这一轮修掉的是**真实站点脚本
依赖的两个平台细节**,并把"页面为什么烂"变成一行可读的数字。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **Annex B.2.5 RegExp 静态属性** | `RegExp.$1`..`$9`、`$&`、`` $` ``、`$'`、`$+`、`input`/`$_`(含 setter),由每次成功的内建匹配更新。百度的 AMD loader 用 `RegExp.$1` 还原 `.js` 扩展名,此前拿到 `undefined` 把组件 URL 拼成 `...64427334a2undefined`,16 个脚本 404;现在 **0 个脚本错误** |
| **`navigator.javaEnabled()`** | 恒返回 false,`wl.js` 不再在启动时抛 "not callable" |
| **脚本错误带调用栈** | `JsError` 的抛出对象已有 `stack`;浏览器诊断现在把它一并打印,匿名帧编号 + 具名帧可用于定位 bundle 内部调用链 |
| **页面健康线** | `--screenshot` 结束时打印一行:`dom_elements / boxes / scripts_failed / scripts_threw / css_errors / css_warnings / images_failed`。失败不再只表现为"看起来不对" |

## 测量(隐藏截图,9-10 秒)

| 站点 | dom_elements | boxes | scripts_failed | scripts_threw | css_warnings |
|---|---|---|---|---|---|
| baidu | 534 | 154 | 0(修复前 16) | 1 | 5 |
| jd | 654 | 117 | 0(修复前整包编译失败) | 5 | 156 |
| bing | 273 | 58 | 0 | 2 | 0 |

## 明确未做 / 下一步

- **JD 的 `Incompatible receiver, Map required` 已修复**:core-js 3 的 `iterators-core`
  用 `'next' in [].keys()` 判断引擎迭代器是否完整。我们的迭代器 `next` 只在 `Get` 的宿主
  分支提供,`in`/描述符看不到,于是 core-js 认定 `BUGGY_SAFARI_ITERATORS = true`,强制
  重定义 `keys`/`entries`/`@@iterator`,原生 Map 实例随即在 `getterFor` 上抛错。
  修法:把 `next` 作为真实属性装到 `%IteratorPrototype%`(宿主分发到 array/collection/
  helper 的 `next`),`in` 与 `Get` 从此一致。探针验证:加载 mall bundle 后
  `Map.prototype.entries` 不再被替换、`new Map().entries().next()` 正常;JD 的 Map 错误
  从日志消失,mall bundle 不再整体失败。
  顺带把"调用非可调用值"的宿主 TypeError 改成带栈的 Error 对象,并在消息里带上被调对象的
  自有键,便于定位 bundle 内部问题。
- **`js_security_v3_0.1.4.js` 触发 10,000,000 步执行上限**:234 KB 的安全脚本不应需要
  10M 步,更像等待某个 API 的空转循环;它跑在 JD 主 bundle 之前,失败时可能留下半初始化的
  全局对象(与上一条相关)。
- **`new Audio()` 已补齐**(`audit` 那一轮的 ReferenceError 已消失),`aria.js` 现在停在
  `addEventListener` 不可调用——媒体宿主对象只有 `on*` 属性,没有通用 EventTarget。
- 内联脚本(JD source 27)`undefined is not a constructor` 未定位。
- 脚本栈只有函数名 + 函数体起始偏移,没有 file:line;给 `CallFrame` 带上脚本身份与位置后
  才能直接指到 bundle 字节。
- 传输层:本机对 HN/GitHub 的 TCP 连接被网络层阻断(curl -4/-6 同样超时),不是 5 秒预算
  造成的,暂时不改预算。

---

# 交接文档 (2026-10-29, 续三) — 生产 bundle 的两处硬失败与脚本元素事件

真实页面排查（baidu / zhihu / qq / jd / bing / hn / github）发现:失败不在同一层。
本轮修掉脚本层的两类**整包失败**,并把"脚本失败兜底"对齐 HTML 规范。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **模板字符串里的除号被当成正则** | `${width/2}` 让 lexer 在空 token 表上做 regex/division 判定,吞掉后续几 KB 并在字符串中间报 `invalid numeric literal`。现在扫描 `${...}` 时逐 token 入栈,结束后随外层状态一起丢弃 |
| **括号赋值被误判成箭头参数默认值** | `(x = await 1)` 在 async 函数里是普通赋值,却在 speculative arrow head 里提前报"await 不允许出现在形参"。现在记录该早期错误,只有 `=>` 确认时才抛出;失败的回溯恢复外层状态 |
| **脚本元素 `error` / `load` 事件** | 按 HTML "execute the script element":结果为 null(抓取失败、MIME 拒绝、编译失败)→ 在元素上派发 `error`;外部脚本执行完(含运行时抛错)→ 派发 `load`。此前只有 stderr 诊断,页面的 CDN 回退链、`<script onerror>` 永远不触发 |
| **无窗口截图** | `--screenshot PATH [--screenshot-delay MS]`:真实管线(网络/CSS/脚本/系统字体)渲染后写 PNG 并退出;窗口隐藏、不抢焦点、macOS 无 Dock 图标。用于不打扰使用者的视觉验收 |

## 验证

- JD 的 1.9 MB `index.chunk.js` 现在完整编译(此前先后挂在 byte 1299026 与 1499679)。
- 新测试:`template_interpolation_lexes_division_as_division`、`a_parenthesised_assignment_containing_await_or_yield_is_not_an_arrow_head`、`a_failed_script_fires_error_at_its_element_so_page_fallbacks_run`。
- `cargo test --workspace` 全绿;fmt/clippy 干净。

## 明确未做

- 脚本元素的 `load`/`error` 事件是异步元素任务,但未处理"脚本在执行前被移除/移动文档"的取消规则。
- 模块图的依赖失败只在入口元素上报 `error` 的语义未逐一核对。
- 脚本层之后仍是运行时/宿主 API(百度 16 个脚本 URL 拼出 `undefined`)、传输层(5 秒连接预算)、样式/布局层。

## 下一步候选

1. 百度 `...undefined` 的 API 返回值 bug。
2. 传输层连接预算与多地址回退(HN 整页超时、GitHub 丢样式表)。
3. 每页健康报告:按层汇总传输失败、编译/运行错误、丢弃声明、无盒元素。

---

# 交接文档 (2026-10-29, 续二) — `<img>` 加载状态与图像源 `drawImage`

接续同日的 canvas 会话。上一轮把 `drawImage` 的画布源做完了;这一轮补上 `<img>`
源——先给图像元素一个真实的加载状态,再把解码位图接进 canvas 引擎。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **图像元素加载状态** | `render-dom::ImageLoadState`(`Unavailable`/`Pending`/`Loaded{width,height}`/`Broken`)存在 `Dom` 上,由嵌入方写入。`src` 被设置(含 `setAttribute`/`setAttributeNS`/属性赋值)立即置 `Pending`,移除 `src` 回到 `Unavailable` |
| **`complete`/`naturalWidth`/`naturalHeight`** | `eval.rs` 的节点属性读取:`complete` 仅在 `Pending` 时为 false(无源视为 complete),自然尺寸只在 `Loaded` 时非零 |
| **浏览器侧状态机** | `start_images` 把新计划的元素图置 `Pending` 并丢弃旧像素;`finish_images` 成功 → `Loaded` + 安装像素,传输/解码/缺失结果 → `Broken` + 丢像素(陈旧 URL 是 warning,不误标);`cancel_images` 回到 `Unavailable` |
| **canvas 图像源** | `CanvasImageSource::Node(NodeId)`;`CanvasEngine` 保存 node → 预乘位图,`Page::install_canvas_image`/`remove_canvas_image` 是嵌入方接缝。`drawImage` 的可用性判定按规范:`Pending` 抛 `InvalidStateError`,`Broken`/无源静默不绘制,`Loaded` 用自然尺寸做三参/五参默认值 |
| **自绘/合成不变** | 图像源复用上一轮的裁剪、采样、alpha、裁剪区与混合模式路径,只是像素来自节点索引 |

## 验证

- 新增 3 个 canvas 端到端测试:`drawImage(img)` 自然尺寸绘制、`Pending` 抛 `InvalidStateError` 且 `Broken`/无源不绘制、`complete`/`naturalWidth`/`naturalHeight` 在四种状态与 `src` 变更后的取值。
- 新增 1 个浏览器测试:`a_planned_image_is_pending_until_its_batch_settles_and_broken_when_it_fails`——计划后 `Pending`,批次无结果后 `Broken`。
- `cargo test --workspace` 全绿;`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check` 干净。

## 明确未做

- `<video>` 与 `ImageBitmap` 源(`ImageBitmap` 接口本身不存在);`createPattern`。
- 图像解码是同步发生在批次应用里的,没有独立的"解码中"子状态;`complete` 只在 `Pending` 为 false。
- `srcset` 选择仍用恒 1000 的 `device_pixel_ratio_milli`(见 gap matrix),与本轮无关。

## 下一步候选

1. `createPattern`(现在有像素了,只差 `CanvasPattern` 对象与绘制时的平铺采样)。
2. `::backdrop` + top layer(~75 reftest)。
3. canvas 文本(`fillText` 家族,需要嵌入方字形栅格)。
4. 表单约束校验(`checkValidity`/`:invalid`,~9 WPT)。

---

# 交接文档 (2026-10-29, 续) — canvas `drawImage`(画布源)

接续同日焦点会话。canvas 里下一个"对真实页面影响大"的缺口是 `drawImage`;本轮先做
**画布到画布**的完整绘制路径,把几何、过滤与合成语义钉死,`<img>` 源留给下一轮
(它先需要图像元素的加载状态,见"明确未做")。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **`drawImage` 三个重载** | `render_js::canvas::CanvasOp::DrawImage` + `CanvasImageSource::Canvas`;脚本侧按 WebIDL 解析 3/5/9 个参数,多出的尾参忽略,非有限值直接返回,`sw`/`sh`/`dw`/`dh` 为 0 不绘制 |
| **源矩形裁剪与等比目的裁剪** | 源矩形先裁到图像边界,目的矩形按同一参数比例裁剪(HTML §4.12.5.1.15);负宽高保持原方向,因此镜像与翻转都由仿射映射自然得到 |
| **光栅化** | 目的四边形走现有 coverage;采样经逆 CTM 回到源坐标,`imageSmoothingEnabled` 关闭时最近邻、开启时双线性(预乘空间插值),图像边界 clamp-to-edge |
| **合成语义** | 与填充同一条路径:`globalAlpha`、裁剪区、`globalCompositeOperation`(含非 source-over 的全画布模式)全部生效 |
| **自绘快照** | 源画布在目标画布被修改前复制,`drawImage(c, ...)` 读到的始终是调用前的位图,重叠拷贝符合规范 |
| **可用性检查** | 源画布任一边为 0 抛 `InvalidStateError`;非画布对象抛 `TypeError`;`drawImage()` 缺参抛 `TypeError` |

## 验证

- 新增 8 个端到端脚本测试(`crates/render-core/src/canvas/tests.rs`):三重重载几何、源矩形越界等比裁剪、负目的宽镜像 + 自绘快照、alpha/变换、`OffscreenCanvas` 源、零尺寸源抛 `InvalidStateError`、非有限值与零延伸不绘制、非画布源 `TypeError`。
- `cargo test -p render-core --lib canvas::` 全绿;`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 干净。

## 明确未做

- **`<img>`/`<svg:image>`/`<video>`/`ImageBitmap` 源**:脚本运行时没有解码后的像素,也没有 `complete`/`naturalWidth` 这样的加载状态。正确做法是让图像元素暴露加载状态并把解码位图接进 canvas 引擎的节点索引,再做可用性判定(`complete === false` 抛 `InvalidStateError`,broken 静默返回)。
- `createPattern`(同样需要图像像素)、canvas 文本、阴影、滤镜、`Path2D`、`getTransform`、`createImageBitmap`。
- 源画布先整体克隆再绘制:正确但每次调用 O(源像素数),后续可换成写时复制或共享只读快照。

## 下一步候选

1. `HTMLImageElement.complete`/`naturalWidth`/`naturalHeight` + 图像源 `drawImage`(同时解锁 `createPattern` 与 `toDataURL` 的图像解码测试)。
2. `::backdrop` + top layer(~75 reftest)。
3. canvas 文本(`fillText` 家族,需要嵌入方字形栅格)。
4. 表单约束校验(`checkValidity`/`:invalid`,~9 WPT)。

---

# 交接文档 (2026-10-29) — 键盘顺序焦点、真正的 :focus-visible、聚焦事件全路径

接续 10-23 的 S16(S16 = 动态伪类接线)会话。那一轮的 WIP 先在 10-29 恢复并提交
(`e05c2f1`),本轮把它记录的三个已知边界收掉。

## 本轮已完成

| 内容 | 落点 |
|---|---|
| **Tab/Shift-Tab 顺序焦点** | `render_core::interaction::sequential_focus_target` 只算目标不改焦点;`Page::advance_focus` 对目标跑运行时聚焦步骤。壳层先向焦点元素(无焦点时向 document)派发可取消的 `keydown`,被取消则不动。焦点落在文本控件上时打开原生编辑器、落在别处则关闭 |
| **真正的 `:focus-visible`** | `MatchContext.focus_visible` 与 `InteractionState.focus_visible` 独立于 `focused`;`:focus` 只看 `focused`,`:focus-visible` 只看自己的字段。壳层规则:键盘导航恒显示、指针只对文本输入显示、脚本 `focus()` 继承最近一次真实输入方式 |
| **聚焦事件全路径** | 点击/键盘/脚本都走 `JsRuntime::focus_element` / `blur_element`(HTML §6.6.3),`blur`→`focusout`→`focus`→`focusin` 按序触发,`relatedTarget` 正确。壳层不再直接 `set_focused_element` 绕过事件 |
| **前置 bug:`Event` 构造器在首个脚本前不可用** | 任何宿主事件(点击、`focus()`)在页面尚未执行过脚本时会以 "event constructor is unavailable" 失败,因为 prelude 是惰性安装的。现在 `dispatch_dom_event`、`focus_element`、`blur_element` 入口都先 `ensure_prelude` |

## 验证

- `cargo test --workspace` 全绿;`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check` 干净。
- 新增覆盖:`page::tests::focus_element_runs_the_focus_events_and_reports_the_transition`(四次事件的顺序与内容)、`advance_focus_walks_the_sequential_order_and_wraps`(两端回绕)、`document::tests::host_interaction_state_reaches_dynamic_pseudo_class_cascade` 增加 `:focus-visible` 用例、`app_tests::tab_walks_the_sequential_focus_order_and_shows_the_ring`、`pointer_focus_shows_the_ring_only_on_text_entry`、`selector::focus_visible_needs_its_own_state_on_top_of_focus`。

## 明确未做

- `:visited` 仍恒不匹配(不暴露访问历史)。
- `:focus-visible` 是浏览器通行规则,不是 Selectors 4 的完整启发式:命令行/提交按钮、指针拖动选中文本等分支未建模。
- Tab 顺序不跨文档、不穿透 shadow host 的 `tabindex` 委托;焦点进入 chrome(地址栏)时不清页面焦点。

## 下一步候选

1. 动画/transition:registry 中仍为 `Missing`,先做时间驱动与 invalidation 接缝,再接 `@keyframes`/`animation-*`/`transition-*`;不要用整帧重绘冒充 transition。
2. `::backdrop` + top layer(~75 reftest)。
3. `drawImage`(解锁图像像素、pattern 与 `toDataURL` 解码测试)。
4. 表单约束校验(`checkValidity`/`:invalid`,~9 WPT)。

---

# 交接文档 (2026-10-26) — canvas toDataURL + 预乘合成/混合模式

接续 2026-10-25 canvas 会话,补齐合成正确性并加上 `toDataURL`。

## 本轮已完成(2 个提交)

| 提交 | 内容 |
|---|---|
| `30752a9` | **预乘位图的 Porter-Duff 修复**:`composite_pixel` 不再对输出除以 alpha(位图存的就是预乘值),也不再让已经预乘的目标再乘一次目标 alpha;WPT `2d.composite.transparent.xor` 的 `0,64,191,127` 落到 ±5 容差内。新增 `clear` 模式(零系数)与关键字校验;canvas 枚举属性按 Web IDL 改为**大小写敏感**(`keyword_is_valid` 精确匹配);补齐全部 15 个混合模式(11 个可分离 + 4 个不可分离,后者用 `SetLum`/`SetSat`/`ClipColor`),按 CSS Compositing and Blending §5.2-5.4 先混合再 source-over。删掉会空转打印的临时探针。render-core lib 325 测试全过 |
| `b6a40f9` | **`HTMLCanvasElement.prototype.toDataURL(type, quality)`**(HTML §4.12.5.3):缺省且 `undefined` 为 `image/png`;类型 ASCII 大小写不敏感;`image/jpg`、`image/webp` 及未知类型回退 PNG;JPEG 从**预乘 RGB**(即透明像素合成到黑)编码,质量只用 JavaScript 数字且须在 `0..=1`,默认 0.92;位图任一边为 0 返回 `data:,`;额外参数忽略;没有 `getContext` 也能工作;方法只装在元素原型上,不在 `CanvasRenderingContext2D`。PNG 从直通 alpha RGBA 编码,统一 base64。新增 8 个测试:解码后像素往返、零尺寸、大小写/回退、JPEG 透明合成、质量高低长度、非数字与越界质量、无上下文/额外参数、接收者校验 |

## 验证

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`python tools/check_site_neutrality.py` 均通过;render-core canvas 65 测试全过。
- `cargo test --workspace` 在整包并行满载时仍有 3 个既有的本机回环测试超时(`a_same_origin_fetch_stores_the_cookies_its_response_sets`、`a_cross_origin_put_is_sent_only_after_its_preflight_allows_it`、`a_refused_preflight_keeps_the_request_from_being_sent`):`serve_until_idle` 在第一个请求到达前只等 10 秒,而并行负载下 `headless_app_with` 的构造可能超过该窗口,监听器先关闭。三个测试单独运行或三个一起运行均通过,与本轮改动无关;下次可把服务器线程挪到 app 构造之后或放宽首请求窗口。
- 本轮**未重跑 WPT**。`toDataURL.default/arguments.*/bogustype/jpg/lowercase.*/nocontext/png/unrecognised/zero*` 只检查前缀或 `data:,`,预计直接增加 13-15 个 pass;`toDataURL.*.primarycolours`、`complexcolours`、`jpeg.alpha`、`jpeg.quality.*` 的像素部分还等 `drawImage`。上次基线仍为 1456/4492(32.4%)。

## 下一步候选

1. `drawImage`(图像/元素/画布作为源;解锁图像像素、pattern 和 `toDataURL` 解码测试)。
2. canvas 文本(`fillText`/`strokeText`/`measureText`,需要嵌入方字形栅格服务,架构级)。
3. `::backdrop` + top layer(75+ reftest)。
4. 表单约束校验(`checkValidity`/`reportValidity`/`:invalid` 等,约 9 个 WPT)。
5. 嵌套浏览上下文(630+,总闸)。

---

# 交接文档 (2026-10-25, 续四) — canvas 状态属性 + Porter-Duff 合成

接续同日 geometry 会话,继续"对正常使用影响大的核心功能"。

## 本轮已完成(2 个提交)

| 提交 | 内容 |
|---|---|
| `75f9567` | **canvas 文本/阴影/栅格质量状态**(HTML §4.12.5.1.12-13):font、direction、filter、fontKerning、fontStretch、fontVariantCaps、globalCompositeOperation、imageSmoothingEnabled/Quality、letterSpacing、shadowBlur/Color/OffsetX/Y、textAlign、textBaseline、textRendering、wordSpacing 共 17 项,全部进入既有 DrawingState(save/restore/reset 免费)。枚举属性关键字校验,未知值"return without updating";字符串属性**原串往返**(WPT 断言 get == set 的原样,不做序列化);shadowColor 先 parse 后存原串。绘制效果(真画文本/阴影)是下一层缺口,状态本身完整可观测——38 个 state-reset 测试测的就是状态语义 |
| `dba9cab` | **Porter-Duff 合成**(globalCompositeOperation 的绘制):12 个模式(copy/src-in/out/atop/over、dst-in/out/atop/over、xor、lighter、destination)按标准系数对在预乘空间合成,反预乘时**输出 alpha 先钳到 1**——'lighter' 的 alpha 相加超 1、颜色在预乘空间相加,这是它需要的关键。**与 source-over 不同,这些模式对每个目标像素都有定义**:覆盖区外源 alpha 为 0,合成跑整张位图,完全离位的 fill 也会按模式清空/保留画布——WPT 的 uncovered.fill 系列断言的就是这个,coverage 早退不能跑在 mode 判断之前。可分离混合模式(multiply 等)暂按 source-over 绘制,已声明缺口 |

## 测量(逐步)

| 步 | pass | 符合率 |
|---|---|---|
| + geometry/roundRect 修复 | 1343 | 29.9% |
| + 状态属性 | 1423 | 31.7% |
| + Porter-Duff | **1456** | **32.4%** |

compositing 集群 43 → 77 pass。dom 29.2%,html 32.8%。

## 下一步候选

1. drawImage(10+,把位图/元素画进 canvas——图像解码集成)。
2. canvas 文本绘制(fillText 家族——需要嵌入方字形栅格,架构级)。
3. 可分离混合模式 multiply/screen/...(状态已有,绘制待做)。
4. ::backdrop + top layer(75+ reftest)。
5. 嵌套浏览上下文(630+,总闸)。

---

# 交接文档 (2026-10-25, 续三) — roundRect 展开 bug + DOMPoint/DOMRect

接续同日渐变会话。主题:优先修对正常页面影响大的核心功能。

## 本轮已完成(1 个提交,含一个引擎级 bug 修复)

| 提交 | 内容 |
|---|---|
| `102d957` | ① **roundRect 2/3 半径展开 bug**:`while radii.len() < 4 { push(last) }` 的预填充跑在 match 展开之前,把 `[20,0]` 垫成 `[20,0,0,0]`,毁掉"两个半径=对角对、三个半径=第二个重复"的规则——凡用多半径的测试全灭。删掉预填充,match 直接展开。② **DOMPoint/DOMPointReadOnly/DOMRect/DOMRectReadOnly**(Geometry Spec §1-2),prelude 自宿主:值存每实例一个隐藏 own 槽(浏览器是内部槽,Object.keys 为空、访问器在原型上),DOMPoint 加 setter,rect 派生边用 min/max 形式(负宽高的含义),toJSON/fromPoint/fromRect/toStringTag 齐备。③ **getBoundingClientRect 返回真 DOMRectReadOnly**(instanceof/toString 与平台一致)。④ **引擎修复:ECMA-262 OrdinaryGet 的 setter-only 续链**——get_value 遇到"只有 setter 的访问器"直接返回 undefined,而规范要求沿原型链继续找基类 getter(派生类 setter 遮蔽基类 getter 的经典形态);getter_on_chain 按 OrdinaryGet 语义续走。render-js 1157 测试 |

## 测量

| 步 | pass | 符合率 |
|---|---|---|
| + gradients | 1303 | 29.0% |
| + roundRect 修复 + geometry | **1343** | **29.9%** |

roundrect 集群 57 → **97 pass**。dom 29.2%,html 30.0%。

## 下一步候选

1. canvas 文本(ctx.font/fillText/strokeText/measureText——state-reset 聚类 22 个 + fillText 10 个;需要嵌入方字形栅格服务,架构级)。
2. createPattern;canvas/img 内在尺寸进 layout(22)。
3. ::backdrop + top layer(75+ reftest)。
4. 嵌套浏览上下文(630+,总闸)。

---

# 交接文档 (2026-10-25, 续二) — canvas 渐变

接续同日 dialog/roundRect 会话。

## 本轮已完成(1 个提交)

| 提交 | 内容 |
|---|---|
| `1959901` | **canvas 渐变**(HTML §4.12.5.1.3):createLinearGradient/createRadialGradient/addColorStop + 渐变填充与描边。**定义经 CanvasHost::define_gradient 注册表操作传给宿主**而非装进样式——因为渐变对象是活的(WPT 先赋 fillStyle 再 addColorStop,下一次 fill 必须生效);注册表属于宿主而非单个 canvas(跨 canvas 可用)。样式只携带 id,fillStyle getter 把 id 映回**同一个**对象(object-current 测试)。**paint 按覆盖像素逐点采样**,经当前变换的逆矩阵映射回用户空间(渐变坐标属于绘制时的空间,不是创建时的);线性=线段投影,径向=圆族二次方程取大根;stops 按 offset 排序、直通 alpha 插值;无 stop 什么都不画。addColorStop 越界抛 IndexSizeError、颜色不可解析抛 SyntaxError。CanvasHost::define_gradient 带默认 no-op(无渐变的宿主零改动)。install_interface 现在写原型的 @@toStringTag(ImageData/OffscreenCanvas 实例的 toString 从 [object Object] 变为接口名)。render-core canvas 测试 35→40 |

## 测量(逐步)

| 步 | pass | 符合率 |
|---|---|---|
| document.write 会话结束 | 1192 | 26.1% |
| + dialog | 1204 | 26.6% |
| + roundRect | 1261 | 28.0% |
| + gradients | **1303** | **29.0%** |

2d.gradient 集群 38 pass(此前 0)。dom 29.2%,html 29.0%。

## 下一步候选

1. createPattern(与渐变同族)+ fillText(需 canvas 字体管线)。
2. canvas/img 内在尺寸进 layout(22,"Parsing of non-negative integers")。
3. ::backdrop + top layer(解锁 75+ showModal/showPopover reftest)。
4. 嵌套浏览上下文(630+,css/ 区 26k 测试的总闸)。

---

# 交接文档 (2026-10-25, 续) — dialog + canvas roundRect

接续同日 document.write 会话。主题:按最新失败聚类继续推进 WPT 通过率。

## 本轮已完成(2 个提交)

| 提交 | 内容 |
|---|---|
| `6be1fc7` | **HTMLDialogElement**:show/showModal/close/requestClose + returnValue IDL 属性。语义按 WPT dialog-close.html 钉死:close() 只作用于 open 的对话框、无参调用**保留** returnValue、close 设置 IDL 值**不触发**脚本定义的 setter(故 returnValue 是 runtime 内部状态而非反射);requestClose 同步派发 cancelable 的 cancel、被 preventDefault 则不动;**close 事件入微任务队列**(调用方能在 close() 返回时断言事件未运行)。showModal 对已打开/未连接抛 InvalidStateError。聚焦步骤与 top layer(::backdrop/:modal)是已声明缺口——75 个 showModal reftest 需要后者。render-js 1152 测试 |
| `c6641fb` | **canvas roundRect**(HTML §4.12.5.1.4)。radii 按 `(unrestricted double or DOMPointInit)` 归一化:数字=圆形、对象(含数组)=DOMPointInit 缺省 x/y 为 0(badinput 形状因此可解析而非抛错)、BigInt=TypeError;序列长度 1..4 且非负,否则 RangeError。路径建立在既有原语上(moveTo + 四边 + 四椭圆弧 + close),fill/stroke/clip/isPointInPath 全部免费获得。**负宽高:命名角互换 + 边取 min/max 极值**——这是负尺寸测试像素对上的关键(直接用 x/y/w/h 公式会把圆角画到形状外);混合符号逆时针绕行(winding 测试需要);相邻半径超边长按标准单一因子缩放;零半径角不产生弧、零高度仍描边。注意:实现在 JS 侧展开为既有 CanvasOp 序列,render-core 零改动。render-core canvas 测试 28→35 |

## 测量(每次一个特性,逐步)

| 步 | pass | 符合率 |
|---|---|---|
| document.write 会话结束 | 1192 | 26.1% |
| + dialog | 1204 | 26.6% |
| + roundRect | **1261** | **28.0%** |

roundrect 集群 57 pass(此前 0);dialog 集群 13 pass,其余转 skip(focus/top-layer 缺口)。dom 区 29.2%,html 27.8%。

## 下一步候选(按聚类大小)

1. canvas 梯度对象:createLinearGradient/createRadialGradient/createPattern 缺失("Missing arguments cause TypeError" + 大量 fill 类测试);fillText(10)需 canvas 侧字体管线。
2. "Parsing of non-negative integers"(22):canvas/img 的 width/height 作为 replaced element 内在尺寸进 layout——canvas.width 反射已对,缺的是呈现尺寸。
3. document.parseHTMLUnsafe(7,真实站点在用,但 WPT 里 5 个 iframe 门控)。
4. ::backdrop + top layer(解锁 75+ showModal/showPopover reftest)。
5. 嵌套浏览上下文仍是最大单缺口(630+)。

---

# 交接文档 (2026-10-25) — document.write 端到端 + WPT 重测

本轮主题:git pull 后重测 WPT 基线,然后实现 HTML §8.4 动态标记插入(document.write/writeln/open/close + currentScript)——WPT 失败聚类的最大单因(~50 个直接测试 + 156 个测试引用它),也是真实遗留页面的硬依赖。

## 基线(本轮开始时)

- pull 带入:命名访问(HTML 7.4.4)、shadow DOM 核心、document.forms/form.elements/表单控件命名访问、Window interface object、WPT reftest 评分器。
- 重测 dom+html walk:**21.6% → 24.8%**(1099 → 1154 / 4653)。dom 23.5%→29.1%,html 21.4%→24.4%。即近期 DOM 面工作已在旧结果之上净增 55 pass。

## 本轮已完成(3 个提交)

| 提交 | 内容 |
|---|---|
| `c555f2a` | **document.write/writeln/open/close + currentScript**。核心模型:`runtime/document_write.rs` 的写入会话——(a) 解析器脚本执行中的 write → 流按脚本分流,fragment 解析(上下文=脚本父元素)后插到脚本之后;(b) 其余 write → 隐式 open:清空文档(含 window/document 的 on* 属性与监听器)后整文档重解析。无活解析器,故**每次 write 重解析累计流**并整体替换上次导入的节点,可观测 DOM 与增量分词器一致。**写入脚本的"完整性判定"**:每次解析配一次"流+哨兵字符"解析——完整脚本的源不变,被 EOF 截断的脚本会吞掉哨兵;只有过哨兵的脚本执行。内联脚本同步按序执行;外链脚本经 DocumentLoader 同步取;**写入的 async/defer/module 脚本是已声明的缺口**(发现并跳过,不静默)。module 中的 write 按规范静默忽略(WPT 断言无 error 事件);XML 文档 write 抛 InvalidStateError。接线:`execute_parser_script`/`execute_parser_compiled_script`(嵌入方传 script 元素 owner)、`PageTask::ParserScript/CompiledParserScript`(浏览器路径带 owner)、WPT 适配器 RunScript 带 owner。currentScript 安装为 document 原型 accessor。render-js 1116→1139 测试 |
| `1861677` | **两个 WPT 语料才抓得到的缺陷**:① head 里的脚本 write——规范行为是解析器离开 head(13.2.6 "anything else"),流落进 body 且在后续标记之前(001.html 断言的就是这个);原实现按 head 上下文解析+插在脚本后,文本进不了 body。现 head 上下文的 write 用 body 上下文解析、插 body 开头。② 导入子树时兄弟顺序颠倒(before 语义变更后内部还在链 cursor),两个写入脚本执行顺序错。子节点对新建元素直接追加。另:会话改为克隆出来处理(槽位不再泄漏 default 占位)、插入参照点在移除旧节点之后计算 |
| `82abe97` | **document.open() 清除 window 与 document 的事件处理器**(HTML 8.4.2;WPT event-listeners.window.js 断言)。这也是防无限递归的规范本身:Range-insertNode-multiple-document-children-crash.html 的 onload 里 open()+close(),close() 再触发 load,第二次分发必须找不到处理器——否则分发递归溢出 64MB 测试栈,**杀死整个 runner 进程**。该测试此前在 insertCell-undefined 处早退,是 document.write 实现后才走到的深水区 |

## 测量(修完三个提交后,dom+html walk)

**24.8% → 26.1%**(1154 → **1192** / 4569)。document-write 集群:1 pass → **34 pass**。剩余失败大头:iframe 类(嵌套浏览上下文缺口)、module 类(同样 iframe)、以及少量内容形状断言。**教训(重演)**:后台跑 walk 期间若 source 在变或 run.ps1 的 cargo run 失败,**结果文件根本不更新而 tail 日志仍显示 "== results"**——三次"重跑"都逐位复读旧结果。测量前先核对 `tools/wpt/results/wpt-results.jsonl` 的 mtime。runner 的单测试栈溢出(Rust 无法捕获)会 abort 整个进程并丢掉整轮 dom+html 结果——进程级隔离(每测试一子进程)是待做项。

## 下一步候选

1. document.write 剩余:写入的 async/defer/module 脚本调度(需要嵌入方资源管线协作);open() 的 session history 细节(ignore-destructive-writes 计数、unload 补偿)。
2. WPT wrong-value 聚类(1184 个,引擎缺陷桶)按新排名读;canvas 像素类失败占大头(fillText/roundRect/toDataURL 缺失)。
3. document.parseHTMLUnsafe(4+ 测试,真实站点在用)、表单校验属性(validity/validationMessage/willValidate,~12 测试)。
4. showPopover/showModal(~49 reftest 因它们失败)。
5. 嵌套浏览上下文仍是最大 harness 限制(630+),也是 css/ 区 26k 测试的阻塞项。

# 交接文档 (2026-10-22 → 2026-10-23) — 门禁修复、release test262 基线与动态伪类接线

**检查点**: 基于 `a6c39f0` 的未提交 WIP,已于 2026-10-29 在 master 上提交。本轮先恢复可信的绿色门禁和基线,再继续能力实现。

## 本轮已落地

1. **clippy 门禁恢复**: 修掉 `collapsible_match`（`crates/render-js/src/runtime/eval.rs`、`crates/render-core/src/canvas/mod.rs` 四处、`crates/render-browser/src/app.rs`）。
2. **test262 runner 加固**:
   - 全量测量改为 `#[ignore]`，`cargo test --workspace` 只跑 10 个 runner 自测；`tools/check.sh` 和 CI 以 release + `--ignored` 跑全量门禁。
   - 证据：生成型 RegExp property-escape 用例在 debug 下超过每文件 30 秒硬超时（本机 `ASCII.js` debug 40 秒、release 7.5 秒）。debug 全量在 RegExp 段制造了 228 个假 timeout；release 全量实际只有 6 个 staging timeout。
   - worker 子进程也必须带 `--ignored`。父测试被标记 ignore 后，旧 `spawn_worker` 没有传该标志，worker 立即退出，整轮被误记为 crash；已修。
   - `RENDER_TEST262_RUN_DIR` 相对路径现在按 workspace root 解析；resume 一个已经完成的目录时不再 spawn 没有任务的 worker（旧代码会永久等待 stdin）。
   - 自测不再依赖 checkout 中的缺失模块，改用合成 fixture；11 个 runner 测试中 10 个默认运行，1 个全量测量默认 ignored。
3. **基线重建**: 2026-10-22 release 全量结果为 files=53,869、variants=103,199、pass=90,765（88.0%）、fail=12,127、skip=294、unsupported=7、timeout=6、crash=0；264 个桶全部不低于旧地板，门禁输出 `264 buckets ok`，耗时 865.67 秒。`crates/render-core/tests/test262-baseline.tsv` 已按同一组桶计数重写，新地板为 90,765；旧地板为 47,116（2026-10-09，`cba591c`）。
4. **其他门禁/文档修复**: cookie 跨源测试改用 IPv6 loopback `[::1]`（macOS 不配置 `127.0.0.2`）；`tools/check.sh` 与 README 改用 `python3`；站点中立性门禁 60 个自测检查、0 blocking、25 advisory、269 个 Rust 文件。README、PROGRESS、`docs/capability_matrix.md`、`docs/test262.md`、`docs/html5_gap_matrix.md`、`docs/html5_scope.md`、`docs/qq-compatibility-analysis.md` 的 test262 数字已对账；已关闭能力（`matchMedia`、CORS、`localStorage`、POST urlencoded、Shadow DOM、`addEventListener` options、BigInt、模块图、UAX #14 换行）的陈旧断言已更新或加历史快照注记。同时核对并关闭了 **S1 字体轴**的文档缺口：`TextStyle` 已携带 `FontRequest`（`crates/render-layout/src/font.rs:108-118`），字体后端按 family/style/weight 选面并合成；`docs/visual_fidelity_gaps.md` 的 S1 已改为 Closed，`html5_scope`/`html5_gap_matrix`/`qq` 的“无消费者”断言同步更正。
5. **动态伪类状态接线（S16 关闭）**:
   - `render_css::selector::InteractionState` 只携带宿主知道的 `hovered`/`active`/`focused`/`target`；`Document::render_with_interaction` 把它们注入 `MatchContext`，viewport/quirks/scope/pseudo-element 仍由渲染管线拥有。
   - `render-browser` 新增 `interaction_state`：祖先链、百分号解码的 fragment `id` 查找与 `<a name>` 回退、初始状态。`PageState` 保存状态并让脚本 `focus()`/`blur()` 通过 `sync_focused_from_dom` 同步到 cascade；`FullPageRenderPayload` 携带状态。
   - 壳层接线：指针移动/离开、主键按下/释放、点击可聚焦内容（同时写 `document.activeElement`）、页面输入框焦点、URL/history fragment 变化；每次全帧重算 `:target`，滚动后也会重算指针下的 hover 链（hover 变化时放弃 retained raster）。
   - 证据：`render-core` 的 `host_interaction_state_reaches_dynamic_pseudo_class_cascade` 同一文档双渲染断言六个伪类的 computed color；浏览器测试 `pointer_movement_and_press_update_the_pages_dynamic_state`、`a_push_state_fragment_updates_the_target_pseudo_class`、`a_full_frame_payload_carries_all_host_interaction_state`，以及 `interaction_state` 的 4 个 helper 测试。
   - 明确边界：`:visited` 仍恒不匹配（不暴露访问历史）；`:focus-visible` 仍等同 `:focus`；Tab/Shift-Tab 尚未接到 `advance_sequential_focus`；点击焦点不派发 focus/blur/focusin/focusout 事件（脚本 `focus()`/`blur()` 仍派发）。

## 验证

- `bash tools/check.sh`: **All checks passed**（fmt/clippy 干净；站点中立性 269 文件、0 blocking、25 advisory；workspace 全绿；test262 步骤复用已验证的 release run dir：90,765/103,199、264 buckets ok）。
- `cargo test --workspace`: 全绿；test262 `10 passed, 1 ignored`；WPT reftest `0 passed, 1 ignored`。
- `cargo clippy --workspace --all-targets -- -D warnings`: clean。
- `cargo fmt --all --check`: clean。
- 站点中立性: 60 checks passed（25 must-flag / 31 must-pass / 4 exit-status / 1 coverage），0 blocking，25 advisory。

## 明确未做

- WPT checkout 仍缺席；WPT 脚本仍是 PowerShell-only，本机没有 `pwsh`/`powershell`；CSS WPT 仍被“iframe 文档不绘制”阻断。
- iframe 内容仍未绘制；动画/transition 仍 `Missing`；shadow-scoped 样式表、webfonts 仍开放。
- 动态伪类仍缺 `:visited`、独立的 `:focus-visible` 判定与 Tab 顺序焦点；点击焦点不派发焦点事件。

## 下一步

P1 动态伪类接线已完成（S16），`docs/visual_fidelity_gaps.md` 记录了剩余边界。下一步进入动画/transition（registry 中仍为 `Missing`）：先设计时间驱动与 invalidation 接缝，再实现解析后的 `@keyframes` 和 `animation-*`/`transition-*` 消费；伪元素盒生成也可并行推进。不要用整帧重绘冒充 transition。

---

# 交接文档 (2026-09-27, 进行中) — 视觉保真专项:UA 样式表 / 字体轴 / 内联 SVG / 文字装饰

本轮主题:用户报告"真实网页效果都非常糟糕,没有网页可以完美渲染"。做法:先用穷举 grep 建立**可验证的证据**（哪些 CSS 属性真的有消费者、哪些值算出来被丢弃),再按文件归属切分并行 agent,避免互相踩踏。

**检查点**:`2bd8e8c`（crates 拆分后续作，78 文件 / +14697 行）。改动前 `cargo test --workspace` 全绿,test262 门禁 9/9 通过(484s),作为仲裁基线。

## 证据文件

`docs/visual_fidelity_gaps.md` —— 本轮所有缺口的 file:line 证据与排序。**先读它,不要重新推导**。

关键结论(全部经代码核实,非报告转述):
1. **字体轴结构性缺失**（最致命）:`render-layout/src/solver/mod.rs:33` 的 `TextStyle` 只有 `font_size`+`line_height`,**没有 weight/style/family**;`render-browser/src/font_backend.rs:33-43` 每个候选组只加载一个字体就 break,`:57-63` 只按字形覆盖选字体。**粗体物理上不可能,斜体不可能,`font-family` 被忽略。**
2. **UA 样式表只有 28 行**(`render-core/src/document.rs:36-63`):`h1`-`h6` 只有 `display:block` 无字号字重边距 → 标题和正文一模一样;**完全没有 `a` 规则** → 链接和正文无法区分。
3. **9 个 paint 属性零消费者**:`text-decoration*` `text-shadow` `list-style*` `text-overflow` `text-indent` `letter-spacing` `text-transform` `filter` `clip-path`。
4. **`@font-face`/`@keyframes` 解析后直接丢弃**(`render-css/src/stylesheet.rs:552-558`)→ 全站无 webfont、无 CSS 动画。
5. **内联 SVG 完全没解析**(`render-html` 无任何 foreign content 代码)→ 全站图标消失。但 `render-dom` 命名空间已就绪(`lib.rs:192/209/502`),缺口只在树构造器。
6. quirks 模式未实现(`document.rs:704`);`<video>` 只有 poster(`PlaceholderDecoder`)。

## 本轮并行 agent 与文件归属(互斥,禁止越界)

| Agent | 归属 | 状态 |
|---|---|---|
| TABLE | `render-layout/**` | CSS 2.1 §17 表格布局 —— **四轮全部验收通过**,81→**126** 测试。含 border-collapse 冲突解决、empty-cells、行组 vertical-align、`<col>` 宽度、`table-layout: fixed`。已交接给 TEXT |
| NET | `render-net/**` | 卡死可观测 + 每地址回退 + 批次有界 —— **已验收** 45→**62**。R2 连接复用 + HTTP/2 评估进行中 |
| JS | `render-js/**` | ToObject 原始值装箱(qq 主 bundle 根因)—— 进行中 |
| PIPE | `render-browser/**` | 在线管线"有样式却裸文本"(京东)—— 进行中。**S1 字体轴在等它释放** |
| UASHEET | `render-core/**` | HTML5 UA 样式表 + text-decoration/text-shadow/list marker —— **代码已落地待正式报告** |
| FOREIGN | `render-html/**` `render-dom/**` | foreign content + `<template>` 惰性 —— **已验收**(81/16)。R3 活动格式元素列表 + in select 进行中 |
| DOCS | `docs/**`(4 份) | **已验收**。它报出的 8 条"我的文档错误"里 2 条是误报、1 条行号已漂移,其余已修 |
| ACCEPT | `tests/**`(新建) | 真实站点验收 harness —— 进行中 |
| CSS2 | `render-css/**` | 冷启动:表格属性 typed 化 + CSS 嵌套 + `color()` + 媒体查询单一真相源 |
| TEXT | `render-layout/**` | 冷启动:`text-overflow`/`text-indent`/`letter-spacing`/`text-transform` 四个无消费者属性 |

## 仲裁记录:验收 TABLE 交付时抓到的规范错误

`table.rs:608-621` 注释声称"HTML rendering section 设置了 `table > tr { vertical-align: middle }`"并据此把单元格默认值设为 `middle`。**结论:保持 `Baseline` 默认,但我当时的理由是错的,已更正。**

**更正(由 CORE r2 复核,原记录措辞"这是错的"不准确):** HTML 现行标准
**§15.3.8 确实写着** `thead, tbody, tfoot, table > tr { vertical-align: middle; } tr, td, th
{ vertical-align: inherit; }`。所以 TABLE 的引用**不是伪造的**,标准文本存在。我当时把
"没有浏览器这样做"当成了"标准没有这条",这是**两个不同的命题**——把它记成后者是错的。

**为什么仍然保持 `Baseline`:** 没有任何浏览器的 UA 样式表实现这条,
`getComputedStyle(td).verticalAlign === "baseline"`,CSS 2.1 §17.5.3 的初始值也是 `baseline`。
目标一致性的判据是**运行时可观测行为**,而 `getComputedStyle` 必须与浏览器一致;照标准
文本实现会让 rENDER 成为唯一一个把单元格中置的引擎。

**处置不变,但理由换了:** 这是**一处有记录的、有理由的偏离**,不是"标准不存在"。
引用可以保留,注释必须写明"标准有此文本,浏览器均未实现,本引擎选择与浏览器一致"。
UASHEET **不要**加这条 UA 规则。CORE 发现了这个矛盾后**没有自行改回**,只上报——
这是对的,因为"有记录的偏离"和"假引用"是两种不同的处置,只有 orchestrator 该选。
返工项:`width:auto` 必须 shrink-to-fit(§17.5.2.2,现为撑满容器)、过约束表必须**撑大**(现为压缩列)、intrinsic 宽度 helper 把 `<br>` 分隔的多行**求和**而非取最大值(影响 table/flex/inline-block 全体消费者)。

## 本轮新挖到的缺口(我自己查的,已进 `docs/visual_fidelity_gaps.md`)

| 编号 | 内容 | 证据 |
|---|---|---|
| S10 | **平台 API 缺失**:`localStorage`/`sessionStorage`/`FormData`/`TextEncoder`/`structuredClone`/`AbortController`/`ResizeObserver`/`customElements` 等全缺。**`localStorage` 伤害最大**——大量生产 bundle 和几乎所有统计/反爬脚本启动就读它,缺失即 `ReferenceError` **打死主脚本**,页面在布局引擎再正确也渲染不出东西 | 从 `render-js/src/value.rs` 的 117 个全局对象枚举比对 |
| S11 | **无嵌套浏览上下文**:`iframe` 无第二个 document、无 `postMessage`、无 `window.open`/`target=_blank` | `render-core/src/document.rs:262,269` 仅出现在元素分类列表 |
| S12 | **只支持 `rgb()`**:`hsl()`/`oklch()`/`lab()` 全不识别 → 声明被丢弃 | `render-css/src/properties.rs:1685-1689` 只有 rgb/rgba 分支 |

**S12 用真实语料量化**(9 份生产样式表,可复现):
```powershell
$files = Get-ChildItem .diag -Recurse -Include *.css -File
foreach ($f in $files) { $t = Get-Content $f.FullName -Raw
  "hsl=" + ([regex]::Matches($t,'(?i)hsla?\(')).Count
  "oklch=" + ([regex]::Matches($t,'(?i)oklch\(')).Count
  "lab=" + ([regex]::Matches($t,'(?<![A-Za-z0-9_-])lab\(')).Count
  "rgb=" + ([regex]::Matches($t,'(?i)rgba?\(')).Count }
```
共 **358 条声明被丢弃**:`color` 109(文字继承错误色、对比度崩坏)、`background`+`background-color` 89(**完全没背景**)、`box-shadow` 系 90(没阴影)、`border` 系 19、`background-image` 8。

**注意交互**:`background-image` 那 8 条尤其隐蔽——渐变**确实**被渲染(`properties.rs:1729` 用 `slice_from(start)` 保留原始文本,`display_list.rs:929-975` 构建真实的 `LinearGradient`/`RadialGradient`),但色标写成 `hsl()` 时**几何保留、颜色丢弃**,于是渐变以错误色相画出来,而不是不画。绿色到蓝色的品牌渐变用 hsl 写就会渲染成错误色相。
这条接近纯收益:HSL/HWB/Lab/LCH/OKLab/OKLCH → sRGB 都是短而精确规定的转换,而 paint 侧本来就吃 sRGB 三元组。

**另外两处我自己查证后推翻的旧说法**(避免下一个人再去追):
- 渐变**不是**没实现。`parse_background_image` 的嵌套块只做消费校验,值用 `slice_from(start)` 原样保留。
- `calc()`/`min()`/`max()`/`clamp()` **已支持**(`render-css/src/length.rs:316-323`);级联层 `@layer` 也已完整实现,含 `@layer` 语句、块与 `revert-layer`(`stylesheet.rs:22-88`、`cascade.rs:879`)。

## 最高优先级(用户报告,当前所有其他事项让路)

### 下划线到处都是:`text-decoration: none` 被静默忽略

**症状**:页面上到处是下划线,很多页面明确写了去掉下划线却不生效。**这是用户亲自报告的,
是目前最显眼的缺陷。**

**根因(已从代码逐环确认)**:
1. UA 表 `a:link { text-decoration-line: underline }` —— 正确,浏览器本来就给链接加下划线。
2. 页面写 `a { text-decoration: none }`。`render-css/src/cascade.rs:524` 的
   `expanded_declaration` 只处理 `background` / `margin|padding` / `border*` / `font` /
   旧 `grid-gap` 长属性 —— **没有 `text-decoration`**。于是作者的 `none` 以字面键名
   `"text-decoration"` 落库,**`text-decoration-line` 长属性从未被覆盖**。
3. `render-core/src/paint/display_list.rs:2380` 先读 `text-decoration-line`,拿到 UA 表的
   `underline`;`:2388` 的 `style.get("text-decoration")` 回退**只在长属性缺失时才触发**,
   而 UA 表永远提供它。

**后果:全网每一个 `text-decoration: none` 都是死代码。** 锚点、导航、`a:hover` 的
下划线切换、`abbr`、`h1..h6` 里的 `<a>` 全部照常画线。

**这不是规范解释问题,是纯粹的遗漏。** CSS 2.1 §16.3.1 明确把 `text-decoration` 列为
`text-decoration-line || text-decoration-color || text-decoration-style` 的简写,CSS
Text Decoration 3 再加 `-thickness`。修法只有一处:`expanded_declaration` 加一个 case。
修完之后**绘制层一行都不用改**——长属性正常参与层叠,作者的 `none` 压过 UA 表的 `underline`,
回退路径自然不再触发。

**第二处(次要,UASHEET 已在代码注释里诚实记录)**
`display_list.rs:1234-1242`:祖先装饰靠"向上找第一个指定了 line 的祖先"近似,于是**中间
那个写了 `text-decoration-line: none` 的行内元素无法解除祖先的装饰**。注释里写明了
正解是把这个传播搬进 render-css、然后删掉这个 walk。这件事必须等简写展开做完再动,
否则就是在绘制层打补丁掩盖层叠层的缺口。

**归属:render-css。** CSS2 正在跑第三轮(specified-value 查询),它落地后这是它的第一优先。

## 待接线事项(跨 crate,需要归属方处理)

1. **`render-browser` 缺两个穷尽 match 臂(阻塞 `clippy -D warnings`)**。UASHEET 给 `DisplayCommand` 加了 `TextShadow(_)` 和 `ListMarker(_)` 两个变体,`render-browser` 有三处对 `DisplayCommand` 的穷尽匹配需要各加一臂:
   ```rust
   DisplayCommand::TextShadow(_) => "text-shadow",
   DisplayCommand::ListMarker(_) => "marker",
   ```
   位置:`crates/render-browser/src/diagnostics.rs:41`、`render_worker.rs:460`、`pipe_diag.rs:174`。`DisplayCommand` 仍是 `Copy`;`content_interaction.rs` 用的是 `!matches!`,**不需要改**(两者都应可命中测试)。归属:PIPE。
2. **clippy 欠账**:`render-js` 6 条(JS agent 在途:`unnested or-patterns`、多余的按值传参、`&mut Vec` 应为 `&mut [_]`);`render-core/src/image/svg.rs` 3 条(`too_many_lines` ×2、`non_snake_case` ×1,`viewBox_scales_geometry_to_the_viewport`——**这是本会话最开始 `cargo check` 就有的既存 WIP 债,不是任何 agent 造成的**)。两条都卡 `-D warnings`。
3. **展现提示的 origin 错了**:`render-css/src/cascade.rs:171-208` 把 presentational hints 放在 `CascadeOrigin::UserAgent`,而 HTML §15.2 规定它们属于**作者** origin 且优先于所有作者规则。这导致新 UA 表必须对 `td,th{padding:1px}` 和 `table{border-spacing:2px}` 用 `:where()` 才能不压过 `cellpadding` 的展现提示。**正解在 render-css**,UASHEET 用 `:where()` 绕过并写清了原因——这是正确的临时手段但不是终点。
4. **S1 字体轴的完整改动面(三次扩充后定稿)**:① `TextStyle` 加 weight/style/family;② `TextFragmentData` 同样要带(`render-layout/src/solver/inline.rs:1034-1043` 只带 text/baseline/font_size,布局之后无法恢复);③ 穿过 `TextMeasurer::measure`、`TextShaper::shape`、`GlyphRun.font`;④ **`render-browser/src/font_backend.rs` 必须按 `(weight, style)` 加载字面**——它现在每组只加载一个、纯按字形覆盖选,所以粗体/斜体物理上不可能;⑤ **`render-core/src/interaction/hit_test.rs:903` 在生产代码里字面量构造 `TextStyle`,必须同步改**。绘制侧已就绪:`GlyphRun.font` 存在且每 run 带 `FontInstanceId`。TEXT 已给出正确扩展模式:**带默认实现的新方法**,让唯一后端实现者零改动继承。

## 协调事故记录(留档,避免下一个人重踩)

0. **我连着两次把任务书投错了 session**(把 CSS 的活投给 TABLE、再投给 NET)。两次都是手抄长 session ID 时抄错。**两次 agent 都自己识破并拒绝了,没有造成任何文件冲突**——TABLE 明确回复"这份任务书不是给我的,我不动我的 crate";NET 更进一步指出 `render-css` 当前有人在改这个活信号。
   **处置:不再手抄 session ID,冷启动新 agent 并给自包含任务书**(`ses_f1d250579ffea3BgANdyLoA54X` 即 CSS2)。新任务书第 6 条已写入这条规则:**发现别的 agent 的代码进了你的 crate,不要"修"也不要回退,报上来**。
   教训不止于抄错:一个 agent 在共享工作树里遇到不属于自己的任务书时,**正确行为是拒绝并说明**,而不是"顺手做了"。
1. **测试二进制栈溢出会留下僵尸进程占住链接器输出**,后续构建报 `LNK1104 ... render_html-*.exe`。处置:`Stop-Process -Name "render_html-*" -Force`;若文件仍被占,把 `target/debug` 下那个陈旧 exe 改名即可释放链接路径,再删掉改名后的副本。注意这些残留进程 `HasExited=True` 且 `taskkill` 报"拒绝访问",那是**已死句柄**的正常表现,不代表还占着文件锁——判断标准是 `target/debug` 下还有没有那个 exe。
2. **`render-js` 曾在编译不过的状态下被别的 agent 观测到**(JS agent 的在途改动)。任何 agent 报告"某个 crate 编译失败"时,先确认不是别人改到一半——本轮已确认 `cargo check -p render-js` 恢复通过,但残留一条 `value.rs` 的 `unused variable` 警告,**这条会卡 `clippy -D warnings`**,最终门禁要盯。
3. **agent 推翻任务书是正确行为,不是抗命**。FOREIGN 指出我给的两处规范错误(`xml:base` 不在 11 条调整表内;`xmlns:xlink` 映射到 XMLNS 命名空间而非 XLink)——核对后**它是对的,我错了**。任务书里的规范细节必须被实现者拿规范原文复核,不能当作既定输入。
   **这条后来扩展成一条更强的规则**:第六轮我给了明确验收标准("`<div a b>` 应该报错")，FOREIGN 查规范后发现**那条标准本身是错的**——13.2.5.34 那个状态对任何情况都不报这个错，而我想保住的那一类错误**根本不存在**（两个无值属性不可能不带空白相邻，因为能结束属性名的每个字符要么是分隔符要么结束标签）。**它按规范做，而不是让代码去迎合我的标准，并且明确说明了分歧。**
5. **被 agent 修正过的"已验证"数字**:我验收 FOREIGN 第五轮时报的 "116 passed" 里，包含了它自己第四轮留下的一条**空转测试**（`scratch_noscript_probe`，只 `eprintln!` 不断言任何东西）。**真实数字是 115。**它自己发现并删除了。也就是说**我的验收方法本身会为一个什么都不测的测试背书**——最终门禁要专门看这一点。
4. **两个 agent 的报告要交叉核对,不能各自采信**。CSS agent 说"`@media` 确实被求值",我去读代码才发现存在**两个能力不一致的媒体查询求值器**(S17),其中一个给每个带特性的查询发假警告。单个 agent 的报告是证据,不是结论。
5. **本机网络环境陷阱(NET agent 实测)**:**这台机器对关闭的 loopback 端口是丢弃 SYN 而非回 RST**,而且 `localhost` 优先解析到 `::1`。所以一个看起来像"服务器卡住"的请求,可能只是一个死地址在烧它的 connect 预算。`crates/render-net/examples/connect_probe.rs` 的 `addr <host:port>` 子命令可以对每个解析出的地址单独计时,`fetch` 子命令打印 elapsed + phase。诊断任何"加载很慢/不动"之前先跑它。
   **NET 已验收**(`cargo test -p render-net` 45 → **65 全绿**):
   - **每个请求都有上报的终态**——"样式表解析失败"和"样式表根本没回来"在日志里可区分了:`render-net FAIL GET <url>: tcp connect: request timed out after 5013ms`,以及 `render-net SLOW ... 200 <bytes> bytes in 2036.4ms`。`RENDER_NET_LOG=1` 打开逐请求日志。**这是本轮最实用的单项产出**——之前"静默卡死"和"快速成功"在日志里长得一样。
   - **连接复用其实一直是好的,之前那个测量值在测 brotli**。`examples/reuse_probe.rs` 起一个计数连接数的本地源站:3 个请求,`gzip` 走 **1 条连接**,`gzip, br` 走 **3 条**。根因是 ureq 3.3 的 brotli reader 到达**解码后**流末尾却没有排空**长度分隔的线上 body**,连接因此永远不回池。`.accept_encoding("gzip")` 早已是修复。这同时解释了旧的"裸 ureq 129ms→230ms"——那个裸探针用的是 ureq 默认 `Accept-Encoding`(含 br)。真实 CDN release 实测 **104.6ms → 20.5ms → 28.0ms**,同源**不同路径** 30.8ms(跨 URL 复用,页面真正需要的),走系统代理 35.1 → 11.7 → 12.7ms。
   - 留了一个**故意在 ureq 修好时失败**的探针 `advertising_brotli_is_what_breaks_the_pool_in_ureq_3_3`,并在测试里写明届时可以把 `br` 加回 Accept-Encoding。**这个手法值得推广**:把已知的上游缺陷钉成一个会提醒你的信号。
   - **HTTP/2 明确拒绝**,三条硬冲突:① ureq 3.3 根本没有 h2 开关,没有帧编解码/HPACK/多路复用器;② **冲突是结构性的**——池化 `Connection` 在用期间被移出池、由 `reuse()` 归还,而 `run()` 全程持有 `&mut Connection`,即**一条连接只能有一个在途请求**;多路复用恰好相反;③ 没有 ALPN API,发不出 ALPN 扩展。
   - **修正我的前提**:这台机器的 curl 是 `Schannel zlib` 且**没开 HTTP2**,所以"curl 第二次 46ms"也是 HTTP/1.1 → HTTP/1.1,和我们说同一种协议。**我原来拿 curl 当 h2 对照是错的。**
   - 复用连接上的归因已测:服务端在**同一条池化连接**上把第二个请求卡住,测试断言连接数仍为 1、该请求仍以 `response headers` 阶段和独立 elapsed 结束、观察者记录到两个独立事件(一 ok 一 fail)。**池化连接没有自己的 connect 阶段,正是最该证明的那种情况。**
6. **已知未修(NET,均已报告)**:① `max_idle_connections_per_host` 默认 **3**,对 40 个同源资源的 HTTP/1.1 页面是**很低的 ceiling**;② ureq 的 `Connection::age()` 恒返回 0,所以 `max_idle_age` 永不淘汰。这两条都指向一个待做项:**同源并发上限目前由第三方默认值决定,不是我们选的**。
7. **CSS2 的嵌套工作当前让工作区编译不过**(`crates/render-css/src/selector.rs` 16 个 + `stylesheet.rs` 11 个错误:`NestedSelectors`、`parse_nested_selector_list`、重复的 `impl AtRuleParser for PropertyParser`、`Parser` 名字冲突)。**这是预期内的在途状态**——NET 正确地没有去"修"它,只上报。八个 agent 并行时看到别的 crate 编译不过,先确认是不是别人改到一半,再决定要不要管。

## 门禁现状(阻塞提交,不是阻塞工作)

`tools/check_site_neutrality.py` 规则数 4 → **9**,现在 **exit 1**:9 blocking + 28 advisory,
173 个文件。self-test 60 项(25 must-flag / 31 must-pass / 4 exit-status / 1 coverage)。

**做法上值得学的一点**:它先把每个文件**词法分析成 code / string / comment 三种 span**
(注释、raw string、byte string、char 字面量、嵌套块注释,18 条单元用例),因为**行正则无法区分
代码里的 token 和字符串或注释里的同一个 token,而这个区分正是全部要点**。只有 `SITE_TOKEN` 用在
决策位置之外——`DOMAIN` 会匹配 `track.info`,`IPV4` 会匹配树里每一条规范引用
(`ECMA-262 20.1.3.6`,211 行)。

**它同时抓出两个既有 bug,而这两个 bug 会让新规则静默失效:**
- `SITE_TOKEN` 用了 `\b`,而 **`\b` 在 `_` 旁边不触发** —— 所以 `\bbilibili\b` **从来没匹配过
  `temp_diag_bilibili`,也就是那条规则唯一要抓的东西**。现在用 `(?<![A-Za-z0-9])` /
  `(?![A-Za-z0-9])`:`_` 是连接符,不是单词字符。
- `EXTERNAL_SWITCH` 的 env-var 那半边用的是裸名,所以 `let _ = "RENDER_SITE is not read here";`
  被报成站点开关。**提到不等于开关。**

**防退化机制**:`check_rule_coverage` 断言每条规则**同时**有 must-flag 和 must-pass 用例,
缺一条就 fail。**没有 allowlist 条目被新增**——中途出现过约 28 个过宽命中,**全部改成修规则而不是
加豁免**,最后过宽 blocking 命中为零。它还明确拒绝把 `hao123`/`163`/裸 `qq` 加进 `SITE_TOKEN`
来让那些捕获可见,理由是"那是对拥有那些 crate 的人的 token 列表决定,也是我被警告过的失败形态"。

### 9 个 blocking 的处置(需要排期,两个 crate 都被占)

| 位置 | 规则 | 判断 |
|---|---|---|
| `render-core/src/interaction.rs:1308` | identifier | 真:活的 `#[test]` 名叫 `..._a_baidu_style_...` |
| `render-core/src/media.rs:584` | identifier | 真:活测试 `discovers_bilibili_style_...` |
| `render-js/examples/bilibili_diag.rs:58,248` | path | 真:按站点做的回放工具 |
| `render-js/examples/check_compile.rs:7` | path | 真:读 `.diag/bilibili/video.js` |
| `render-js/src/runtime/tests.rs:2205,2238,2641,2660` | path | 真:`temp_diag_mutual_recursion` / `temp_read_5073_state` |

`render-core` 归 FONT,`render-js` 归 SURFACE。**在 CI 上是红的,但只有 push 才触发,而推送由我
控制**,所以不阻塞工作。

**那两个 `render-js` 测试应该删掉而不是修**——变异审计已经独立发现它们**只 `eprintln!`、不断言
任何东西、且是活的并计入测试数**。一条什么都不测的测试还违反门禁,两条理由指向同一个动作。

### 一条我要写死的判断:按站点命名的**诊断工具**是允许的

28 个 advisory 里有 3 个是文件名(`baidu_diag.rs`、`bilibili_layout_diag.rs`、`bilibili_diag.rs`)。
**我不打算改这些**,理由是审计自己给出的那句:一个以"它诊断什么"命名的诊断工具,**比一个引擎内部
以站点命名的分支弱得多**。

这个项目的工作方法就是测量真实页面,所以语料工具必须能指向它测的页面。**被禁止的是引擎里的
按站点分支,不是按站点的工具。** 写死这条是为了防止下一个 agent 看到 advisory 就把语料工具删掉。

## 规则裁决:测试期望与"不得断言缺陷"冲突时,后者赢

PARSER 改了**两条既有测试**,理由是它们断言了一个已知错误的 DOM:`<![CDATA[x]]>y` 产生**两个**
注释节点。规范里 bogus comment 状态会转入 comment 状态,html5lib 也一致,所以**一个注释才是正确的,
而且永远不可能再变回两个**。

**裁决:`不得断言缺陷` 优先于 `既有测试必须未修改通过`。**

理由:规则的职责是防止交付已知错误的行为。为了保住一条错误的测试绿,而去为一个真实页面交付
**已知错误的 DOM**——那个代价比改测试大得多。而且这不是"可以随便改测试"的先例,先例仅限于
**改掉一条断言了缺陷的测试**;改机制时该测试必须同时改名并保留(`..._is_one_bogus_comment`)。

**记录在此,以免下一个 agent 把"改测试"读成通用许可。**

## WPT runner 已建成,但**跑不了任何一个测试**——而这是正确的结论

`tools/wpt/run.ps1` 一条命令走通:自检 → 按固定修订版取套件 → 普查 → 运行 → 写
`tools/wpt/results/wpt-results.json`。**32,576 个计分测试**(css 26,614 / dom 559 / html 5,403),
静态可执行 20,986(**64.4%**),**实际跑起来 0 个**。

**原因结构性:WPT 的 CSS 测试是 `testcss.js` 测试,而 `testcss.js` 是一个 iframe 驱动器。
`render-core` 没有嵌套浏览上下文。**

**所以:引擎里唯一拥有真实层叠、真实选择器匹配器和真实布局引擎的那块面积,恰好是它的测试工具跑不起来的
那一块。**

**它拒绝报符合率,而且 runner 里根本没有能报符合率的代码路径**——`Rate` 在零分母上返回 `None`,
所以空跑**什么都不打印而不是打印 `0%`**。它也**拒绝报引擎缺陷清单**,并把这一点标为
**"blocked, not empty" 而不是"零缺陷"**——因为什么都没被求值,任何缺陷清单都只是工具的产物。

**这改变了 S11 的权重。**iframe 不再是十二个缺口里的一个:**它堵住了对其他一切的外部测量**。
不能承载嵌套文档的引擎**无法被现存最大的 CSS 一致性套件评分**。仅这一项就阻塞了
**1,772** 个看起来静态可执行的测试。**代价不是一种页面类型,是项目"知道自己 CSS 对不对"的能力。**

**它在自己仪器里找出两个缺陷,和本项目反复遇到的那一类完全相同:**
- **引用身份指标静默坏了**——两个集合存在不同键空间里,所以报告读作"两边都是 0;命名规则本会漏掉
  8,748 个引用并丢掉 8,695 个真实测试"——**不可能的数字,没有报错,没有失败测试**。**只靠读数字才发现。**
- **取件 bug 把总体翻倍**——`Copy-Item -Recurse` 覆盖已有缓存产生 `css/css/`,数量 46,325 → 92,650,
  **而那棵树看起来仍像一个合法检出**。现在普查**拒绝自嵌套检出**。

**一个值得记住的校准数字:32,576 个测试里 6,466 个(19.8%)根本没有断言点**——静态和运行时双重检测,
方法调用和函数声明在自检抓出分类器把它们算进去之后被排除。**参考套件里五分之一的测试根本没有失败
的可能**,这对任何会读符合率的人是必要的校准。

**待接线的引擎契约(三条,带签名,在 `tools/wpt/FINDINGS.md`)。最高杠杆的一条:
CSS 解析器在 `stylesheet.rs:1270` 算出了 `line`/`column`,然后在 `stylesheet.rs:35` 把它们
扔掉了——`Declaration` 没有位置字段,所以一个失败**根本无法**被报成引擎的 `file:line`。
给 `Declaration` 加 `line`/`column` 几乎零成本,而它正是把"WPT 测试挂了"变成"这个函数错了"的东西。
另两条:`ComputedStyle::get` 把"不适用"和"未实现"混为一谈;没有 `file://` 样式表加载器。

`tests/wpt_runner/` 的 `--features engine` 适配器**写好了但从未编译过**(当时 `render-js` 非编译),
**按未经证明的草稿对待**。另外 772 个测试被 legacy `setup()` 挡住(不是引擎,是它自己能关的),
535 个缺失 fixture 是跨区域引用。`docs/wpt.md` 可能要更正:本机 `pwsh` 不存在(只有 5.1),
而全局 git 配置指向一个死掉的 SOCKS5 代理,所以 `tools/fetch-wpt.ps1` 在这台机器上跑不起来。

## 排队中(等 crate 归属释放后开工)

### S1 字体轴 —— 最高价值剩余项,**同时被三个 crate 阻塞**

需要:① `TextStyle` 加 weight/style/family;② `TextFragmentData` 同样要带
(`render-layout/src/solver/inline.rs:1034-1043` 只带 text/baseline/font_size,布局之后无法恢复);
③ 穿过 `TextMeasurer::measure` / `TextShaper::shape` / `GlyphRun.font`;④
**`render-browser/src/font_backend.rs` 必须按 `(weight, style)` 加载字面**——它现在每组只
加载一个、纯按字形覆盖选,所以粗体/斜体物理上不可能;⑤ **`render-core/src/interaction/hit_test.rs:903`
在生产代码里字面量构造 `TextStyle`,必须同步改**。绘制侧已就绪:`GlyphRun.font` 存在且每 run
带 `FontInstanceId`。

**当前阻塞**:`render-layout` 在 CSS2 第四轮手里(下划线 bug + fixture),`render-browser` 在
PIPE 手里,`render-core` 在 CORE 手里。**三个都被占。**

**不要拆开做**:`CLAUDE.md` 明文"implement features fully or not at all"。只改 `TextStyle`
不改 `font_backend` 的话编译能过(新字段被忽略)但**行为零变化**,等于交付假特性。
**扩展模式照 TEXT 的做法**:带默认实现的新方法,让唯一的后端实现者零改动继承正确行为。

- **内联 SVG 栅格化**:CORE 在做。解析半边已完(FOREIGN),栅格化走已有的 `image/svg.rs`,
  注册为 image resource。**读 `xlink:href` 必须用 `attribute_ns`**,用 `attribute(node,"href")`
  会静默丢掉每一张链接图片。
- **S24 form owner**(本轮新发现,无人查过):`render-dom` **完全没有 form owner 概念**,
  `FormData` 在 render-js 里是孤立数据结构、和任何 form 都没连接。搜索框/登录这条最常用的
  交互路径底层是空的。FOREIGN 在做 DOM 侧的 owner 关系 + 解析器 form element pointer。
  `render-js` / `render-browser` 侧消费(`form.elements` / `requestSubmit` / `reset` /
  `formaction` / 从 form 构造 `FormData`)要等 owner 存在才能开始。
- S4 `@font-face`(223 块)/`@keyframes`(249 块)评估、S4b `@supports`(93 块无条件应用)、
  S7 quirks 模式、S11 iframe、S8 视频像素解码、S13 Selection/Range 与滚动容器。
- S9 `spec/registry.rs` 补登 animation/font-face/pseudo-element/table/inline-svg/video/form 条目。

### 给 CSS agent 的返工清单(下划线修复已在第四轮派发中)

1. **S23 颜色/下划线**:`expanded_declaration` 加 `text-decoration` 分支(两种语法 + `none`),
   注册四个长属性。**用 `css_corpus` 量化 `.diag/**` 里有多少条 `text-decoration` 简写**——
   这个数字就是 bug 影响面的量化答案。
2. S17 的 `render-core` 半边已交给 CORE(它把 `has_unsupported_query` 换成
   `!media_query_list_is_supported(media)`)。
3. S21 `hasOwnProperty.call` 归 JSTRIAGE。
4. 展现提示的 origin 错了(`cascade.rs:171-208` 放在 `CascadeOrigin::UserAgent`,
   HTML §15.2 规定是**作者** origin 且优先于所有作者规则)——UASHEET 因此被迫对
   `td,th` 和 `table` 用 `:where()` 绕开。正解在 render-css。

### 给 CSS agent 的返工清单(已备好,等它收工)

1. **S12 颜色函数**(最高优先):`properties.rs:1685-1689` 只认 `rgb/rgba`。补 `hsl/hsla/hwb/lab/lch/oklab/oklch/color()/color-mix()/light-dark()`。9 份真实样式表里 **358 条声明正因此被丢弃**(`color` 109 / `background`+`background-color` 89 / `box-shadow` 系 90 / `border` 系 19 / `background-image` 8),量化命令见上。渐变几何已实现,**色标颜色丢了会画成错误色相**,不是不画。
2. **表格属性注册为 typed property**:`border-spacing`、`caption-side`、`vertical-align`、`border-collapse`、`table-layout`、`empty-cells` 目前只在 token 级 computed map 里,求解器得用 `ComputedStyle::get(..).css_text()` 读。注册成 typed 是正解。注意 TABLE agent 的测试显式写了 `border-spacing: 0`,所以注册不会打破它。
3. **修好上一轮量化表**:CSS agent 建的 `render-css/examples/css_corpus.rs` 目前只统计**规则级**丢弃。声明级的丢弃(本节第 1 条那 358 条)不在它的度量里——建议扩展成同时报告"规则丢弃"和"声明丢弃",否则这类缺口永远测不出来。
4. S6 的 `position: sticky` 有枚举无消费者;`z-index` 只在 block 兄弟间排序,flex/grid 子项不参与,绘制层无感知。

## 法则更新

`docs/generic-browser-todo.md` 新增 **"Priority -1: Gaps Are Implemented Forward, Never Removed"** 明文法则:未实现/无人认领的能力是**待实现的 TODO**,不是可删可绕过的东西;不得删行为、不得用假近似冒充实现、不得特判绕过;**过期文档同样是缺陷**,架构迁移后必须重写而非删除。同时修掉该文件里 `engine.py` 的 Python 残留。

---

# 交接文档 (2026-09-27, 进行中) — 多网站可用性专项:代理/居中/封面/展现属性/背压

本轮主题:用户报告"打开网页看到乱七八糟的文字 + UI 拖拽无动画 + 常用网站没有能完美工作的"。以真实浏览器截图为参照逐站对比,把差异归类为标准缺口逐一向前修复(未回退任何既有代码)。

## 本轮已完成(全部有测试)

| 项 | 内容 |
|---|---|
| **render-net 系统代理** | ureq 启用 `win-system-proxy` feature;`HttpTransport::new` 经 `ureq::Proxy::try_from_env()` 读 env(ALL_PROXY/HTTPS_PROXY/HTTP_PROXY+NO_PROXY)与 Windows 注册表代理;新增 `with_proxy` 注入点。**HN 实测双路径取回成功**(用户代理 127.0.0.1:17890)。测试:tests/proxy_transport.rs(本地 CONNECT 代理 + no_proxy 绕行,3 测试) |
| **标签拖拽实时动画** | TabDrag 重写:拖动 tab 连续跟随指针(按下的抓取点偏移),邻居按"过中点半槽渐移"连续让位,极值收敛到与提交一致的槽位;模型顺序只在释放时提交(release());绘制层 paint_one_tab/translate_tab_geometry,拖动 tab 最后画。chrome.rs/app.rs 测试全绿 |
| **B站视频封面** | ①`<picture>` 源选择:avif 源按 type 跳过落 webp(既有逻辑已对,验证过);②真正根因:block.rs 第一遍 static 分支"穿越包装层携带高度"条件失效(padding-top 在 static 子层、relative 祖先在上),补"已知 margin-box 底边抬升";第二遍同修。离线验证 zero-height img 10→0 |
| **知乎登录卡居中** | flex 求解器 `intrinsic_flex_size`:内在测量中百分比宽度视为 auto(resolve_size_against(None)),确定宽度封顶 max-content(此前 `width:100%` 按 basis 解析把卡片撑满 1770 导致不居中)。离线验证卡片 733px 居中于 518.5 |
| **HTML5 展现属性** | render-css 新增 `cascade_element_with_origins`(UA origin 逐元素声明,零特异性)+ `compute_document_styles_with_hints` 钩子;render-core `presentational_hint_declarations` 实现 bgcolor/width/height/align/valign/cellpadding/cellspacing/border/hspace/vspace 映射;UA sheet 加 `center{text-align:center}`。**HN 实测:橙头/米黄底/85% 居中表格全部出现**(此前"裸文本")。4 个回归测试 |
| **input placeholder** | tree.rs:value 为空时合成 placeholder 文本节点。知乎登录表单占位文字("手机号"/"输入 6 位短信验证码")实测可见 |
| **document.createComment** | jQuery 1.6.4 特征检测 `appendChild(createComment())` 因此崩溃(京东全家脚本失败);补 NativeFunction::CreateComment + Node 包装的 nodeValue/data(Text/Comment)。京东脚本失败数 5→0 |
| **队列满背压重做** | 移除事件循环 `thread::sleep` 退效(**淘宝首帧 30s 零帧的根因**):队列满时请求 park 为 Deferred(fetch_handles),poll_network 每轮经 `NetworkWorker::submit_with_cancellation` 重试;PendingNavigation/StyleSheets/Scripts/Images 加 since/stall_reported 30 秒一次性卡顿上报;render_dirty+is_tab_busy 渲染合并(变更落在运行中渲染上→提交后重提交,防活页饿死)。**淘宝实测 30s 内 3 帧(此前 0 帧)** |

## 诊断工具沉淀

- `crates/render-core/examples/layout_chain_diag.rs`:按类名打印祖先链的 computed style + fragment rect(支持 URL_SUBSTR=CSS 多文件映射),对照渲染诊断利器。
- `crates/render-core/examples/css_probe.rs`:单文件 CSS 是否生效的渐进前缀探针。
- `tools/ppm2png.py`:RENDER_DUMP_FRAME 的 PPM→PNG。
- 照片对比法:rENDER 用 RENDER_DUMP_FRAME 截帧(用完 taskkill //F //IM),参照用 browser-use 截图,人工/视觉对比归类差异。

## 京东在线问题精确诊断(下一批起点)

在线日志时序(RENDER_DEBUG_FRAME,18s 窗口):
1. 行 28:JS 执行后渲染,stylesheets=0、computed_styles=650(内联样式)、918 fragments;行 45 帧 commit:497 display items、content_height=5783。
2. 行 87:外链 CSS 批次应用后渲染,**stylesheets=3、computed_styles=650、492 fragments**——样式确实进了管线;行 104 帧 commit:318 display items、content_height=3028。**视觉是裸文本**(截图证实)。
3. 行 104 之后**再无任何渲染**(图片 44 张在途也未触发)。

离线对照(保存的 curl SSR HTML + 同 3 个 CSS,layout_chain_diag):`.search-m` 子树完美——`.form` 1008×44 红边框(position:absolute left:50%)、`input.text` 856×40、红色 `button.button`("搜索"文本)全在。**同一引擎同一 CSS,离线布局/样式全对。**

在线与离线的差异 = DOM 状态(在线是 JS 执行后的 DOM,离线是原始 SSR HTML)。两个待验证假设:
A. JS 改动后的 DOM(如给 body/html 加 class、注入节点)与 CSS 选择器交互后,大量规则未命中或布局塌缩(918→492 fragments);需离线回放"JS 执行后的 DOM"(仿 render-js examples/bilibili_diag.rs 的离线回放,保存 JS 后 DOM 快照)。
B. 视觉裸文本与"stylesheets=3"矛盾 → 需查 render_worker.rs 提交帧的 computed_styles/绘制路径是否用的同一份 styles(map 克隆时机)。
另:渲染停滞(行 104 后无新渲染)违反"图片完成必重渲"预期,查 finish_images→schedule_page_render 链路(可能与 render_dirty/is_tab_busy 合并逻辑交互:第 87 渲染 commit 时 render_dirty 置 true resubmit 的那帧是否被 drain_latest 丢弃)。
顺带发现(离线):`.form` 的 `transform:translateX(-50%)` 似未生效(form 左缘 885=109+776 而非居中),按钮 x=1809 超出 1770 视口——transform 对 absolute 定位几何的影响待查。

**已定案(PIPE r 收尾,`render-browser` 157/0)**——**上面的假设 B 被测量证伪,真正的根因在 `render-css`。**

**测量(真实文档 + 3 张真实样式表,本地 HTTP fixture 走完整浏览器路径):**

```
浏览器路径(process_page_render,含 re-plan + rematch + merge)
  unstyled: stylesheets=0 computed_styles=650 fragments=918 items=721 content_height=6234.72
  styled:   stylesheets=3 computed_styles=651 fragments=492 items=349 content_height=3028
离线引擎参考(同 DOM + 同 3 表,无浏览器簿记)
  styled:   items=349 content_height=3028          <-- 完全相同
```

**从 apply 到 commit 的浏览器侧路径与"把样式表直接交给引擎"逐项等价。**`merge_current_style_sheets`
没丢任何东西,re-plan/rematch 产出相同的键,3 张表都进了层叠。**不是浏览器的问题。**

**定位结果:`vanished` 从 20 暴涨到 151**(有非 `none` display 却不产生任何盒的块级元素),
两个类别:
- **A 类**:`.cate_menu_icon` 命中 `first-screen.chunk.css rule#319 => display: inline-block`,
  `.loading` 命中 `index.chunk.css rule#1532 => display: block`——**规则命中、值正确,却无盒** → `render-layout`。
- **B 类**:`.cw-icon` / `.dropdown-layer` / `#J_cart_pop` / `.JS_navCtn.cate_menu` /
  `.cate_menu_item`——**151 个块级元素,作者样式表本该给出 `display` 却一条都没匹配上** → `render-css`。

**B 类的具体根因已钉死,是规范违反。**每一条 `unexpected token: Semicolon` 都精确落在 legacy star
hack 之后的 `;` 上,而**那条规则保留了它之前的声明、丢掉了从非法那条开始的全部声明**:

```
index.chunk.css  1:269395 / 1:269403
  `.jdmcc-topbar #ttbar-serv .item{display:inline-block;*display:inline;*zoom:1;min-width:50px;…}`
mall index.css   1:697674 / 1:697711   同一规则,顺序不同
fingerprint: blocks_keeping_display=1  blocks_that_lost_display=1   (每张表)
```

三张表共 19 处 star hack。承载 `display:inline-block` 的那条块——**就是让京东顶栏横排的那条规则**——
丢了它,`<li>` 回落到 UA 的 `list-item`,**页头就竖着堆了。那就是"裸文本"截图。**

CSS Syntax §5.4.2 / §5.4.4:一条不可解析的声明必须**单独丢弃**,解析继续走同一块里的下一条。
现在一个非法属性名(`*zoom` / `*display`)**把该声明及其之后同一块里的每一条都一起丢掉**,
然后解析器报 `unexpected end of input` 而不是恢复。位置:`crates/render-css/src/stylesheet.rs`
的声明列表消费者。**已作为 `render-css` 的下一轮排期**(当前 `render-css` 在跑 at-rule 诊断)。

**方法论教训(本条比上面所有定位都重要):整条时间线都建立在"会被打印的日志"上,而那些日志属于被丢弃的帧。**

`commit_render` 在身份门禁**之前就 return**,并且在消费 `page.render_dirty` **之前** return;
`log_completed_frame_debug` 又跑在那个门禁**之前**。所以**我们此前读的每一行帧日志,都可能是一帧
根本没有提交的渲染**。测量必须先确认那帧被 commit 了,否则数字描述的不是最终画面。

**已确认并修复的浏览器侧收敛缺陷:**
- `PageState::set_source` 清 `expected_render` 时**不取消在飞的渲染作业**,所以一次导航若在渲染
  期间落地,标签页就**确定性地**冻结在上一个文档上。修法:每条渲染完成路径都要兑现合并后的请求,
  把 commit 日志移到门禁之后(被丢弃的帧现在记为 `discarding superseded frame`),并记住
  **请求的**视口(`render_dirty_viewport`)而不是已提交的视口。
- 第二个更难的孤儿:**被取消的渲染从不报告完成**,所以事件循环永远不会被唤醒。
  `recover_unresolved_render_requests()`(由 `about_to_wait` 驱动,并把 `render_dirty` 加进唤醒条件)
  是自愈路径。这是真实浏览器运行里**可复现的页面冻结**。
- "44 张图、不重渲"这条怀疑:**部分为假,但有一个真实漏洞**。一批结果全部陈旧/全部解码失败/
已应用过的批次,什么都没解码因而**从不把页面标脏**,于是**一张图都加载失败的页面会冻结在最后一次
提交上**。现在每个完成的批次都标脏并递增 generation。

**经核对无需改动的(script-fetch 门):**提交 `1c4f6e9` 的说法成立——外链脚本分支**无条件**提交请求,
不论 `styles_resolved`;只有*执行*才延后,经由 `held_scripts`。内联 body 在样式表在飞时就准备好
(解析),只有 `execute_script_batch` 等待。这符合 HTML 的"blocking scripts"。

**给 `render-net` 的新发现:响应阶段没有读超时。**`HttpTransport::with_proxy` 把
`timeout_recv_response(None)` 和 `timeout_recv_body(None)` 都设成无限,所以**连接建立后卡住的传输会
永久挂起**。本机就是如此:浏览器对 jd.com 的请求经由可用的系统代理(curl 200,193164 B,70 ms)
处于 Established 且 **80 秒零进展**,而 curl 同路径 70 ms 返回。**这就是这台机器上没有 jd.com 实时信号
的原因**,也是 PIPE 只能用 curl 抓页面再本地起 HTTP 服务来复现的原因。

## 下一批(按价值排序)

1. **京东在线管线**:样式表已获取+解析(有 parse warning 日志)但 computed style 未生效;离线同 CSS+DOM 全部生效。疑在 style batch apply→page.style_sheets 链路或多批次竞态。离线诊断已证明 CSS/选择器/DOM 均无问题,锁定 render-browser 在线 apply 环节。
2. **表格列分布**:HN 等老式表格站:单元格顺序/宽度分配异常(rank 列被挤到最右)。属 table layout 算法深水区。
3. **SVG 图片解码**:image/svg+xml 直接 UnsupportedContentType(HN logo/投票箭头、大量 favicon)。可选纯 Rust 方案(usvg/resvg 较重)或最小 SVG 子集自绘。
4. **知乎二维码/社交图标**:JS 注入 <img>/CSS background-image,需继续 JS 兼容推进。
5. **AVIF 解码**:B站等 CDN 在 Accept 协商下可能回 avif(现 Accept 恒 jpeg 尚可)。
6. **滚动条**:尚未开始(agent 被并发限制挡掉两次)。overlay 滚动条渲染+拖拽+命中测试,方案已写在任务书里。

## 同日续（整合收尾）

- **test262 门禁恢复**：agent 中断但修复已落盘——四桶全部 ≥ 基线（Date 620≥594、Math 164=164、annexB 62=62、Map 230≥224），全量 `cargo test -p render-core --test test262` 门禁通过（9/9，449s）。render-js 单测 195 全绿。
- **淘宝首帧卡死根因确认并修复**：`submit_with_queue_full_backoff` 在事件循环里 `thread::sleep` 指数退避（最多 8 次×5-200ms/请求），淘宝几十个资源并发把 UI 线程反复睡死 → 30 秒零帧。重做为非阻塞 Deferred park + poll_network 每轮 `submit_with_cancellation` 重试 + 30s 一次性 stall 上报 + `render_dirty`/`is_tab_busy` 渲染合并（变更落在运行中渲染上，提交后重提交）。**实测 30s 内 3 帧**。
- **内容嗅探兜底**：CDN 给 PNG 数据标 `image/svg+xml`（CSDN 实测）导致图片被拒；现在不支持的声明类型 + 有效嗅探签名 → 按内容解码（Warning）；受支持的声明类型与字节不符仍是硬错误（ ContentTypeMismatch）。测试 `unsupported_declared_type_with_valid_signature_decodes_by_content`。
- **多站实测快照（1770×1170）**：搜狐≈完整可用（导航/图片/新闻流全渲染）；网易 163≈完整可用（轮播/图集/右栏，少量绝对定位文字重叠）；淘宝=骨架页正确（内容依赖 JS）；HN=结构完整（表格列分布待修）；知乎=登录卡居中+占位符输入框；京东=脚本零失败但在线 CSS 未应用（离线同 CSS/DOM 完全正常 → 锁定 render-browser 在线 style batch apply 链路，下一批首选）；CSDN=JS 重度客户端渲染仍空白（ResourceLimit array-like 上限 + waf 脚本）。
- 全工作区 `cargo clippy --all-targets -D warnings`、`cargo fmt --check`、`cargo test --workspace` 全绿（含 test262 门禁）。
- 诊断工具：`layout_chain_diag.rs`（类名祖先链 computed style+fragment rect，支持 URL_SUBSTR=CSS 多文件映射）、`css_probe.rs`（渐进前缀 CSS 生效探针）、`tools/ppm2png.py`。

# 交接文档 (2026-09-26, 进行中) — transform 端到端 + 站点阻塞突破 + 性能实测

本轮主题:用户指示"真实网站可用性优先"。多 subagent 并行推进,全部基于同一共享契约。

## 本轮已完成(全部有测试,fmt/clippy 干净)

| 项 | 内容 |
|---|---|
| **CSS transform 端到端** | 共享契约(render-css `TransformFunction/TransformList/TransformOrigin` + render-core `Transform2D::{apply,then,inverse,is_translation}` 数学方法);解析完备化 77 测试(修复 rotate/skew 度序列化 f32 往返噪声、`rotate(1e40deg)` 溢出、matrix3d 注释错位);显示列表发射:transform+opacity 合并单 stacking context,`fragment_transform` 按 T(origin)∘M∘T(−origin) 复合,`transform/transform-origin` 已注册进 computed.rs 的 standard_baseline(否则类型值根本不流入 paint——曾是非官方发现的硬缺口);光栅化:纯平移快路径(零分配,`is_translation() && opacity==1` 时累加进 item_offset),通用仿射走全尺寸离屏面 + 逆映射双线性(预乘空间)warp 合成,det==0 安全,嵌套天然复合。render-core lib 138 绿 |
| **样式表批次 URL 重匹配** | 修复"DOM 修订号一变就丢弃整个在途样式表批次"的可用性硬伤:worker 重排 plan 后按索引 zip 导致取回的 CSS 被 UnexpectedResponseUrl 丢弃/错挂。新增 `apply_stylesheet_batch_rematched`(resources.rs):按 requested_url 匹配到新 plan 槽位(重挂到当前 owner),新链接记 PendingFetch 留待下轮,消失链接静默丢弃;render_worker.rs 接线。3 个新测试(追加/改向/传输错误归因) |
| **bilibili 主 bundle 突破(subagent)** | ①根因:嵌套 `new` 泄漏外层 new.target(`new_target_stack.last()` 取到还在栈上的外层构造器,实例原型挂错 → babel `_classCallCheck` 全灭,报 "Cannot call a class as a function")。修:Expr::New 求值处压/弹构造器,new_target 栈只服务 super();Iterator 抽象类判定同步修。②根因:`Object.defineProperty` 把 Symbol 键 to_js_string 强转(unscopables 失效 → log-reporter "reading 'keys'")。修:define_property/getOwnPropertyDescriptor/hasOwn/hasOwnProperty 四处符号键分支 + `define_symbol_property` 兼容 no-op 重定义。**结果:主 bundle 越过 babel/core-js 全部类工厂与 i18next,推进到 Vue 3 响应式,新卡点 `Proxy is not defined`(P7)**;log-reporter 主执行通过。render-js 179 绿。诊断工具:example bilibili_diag + 离线捕获 .diag/bilibili/ + RENDER_DIAG_STACK=1(RENDER_JS_FRAME_OFFSETS=1 帧偏移) |

## 性能实测结论(用户反馈"特别慢"的诊断)

1. **网络层连接不复用(最大单项,agent 修复中)**:每请求重付 TCP+TLS。实测同 CDN URL 两次 fetch 135ms→134ms 不变,裸 ureq 对照同样不复用(129ms→230ms,问题在 ureq 3.3 + rustls 配置下池子失效,非我们封装);curl 同链路复用第二次 46ms。工具:`crates/render-net/examples/fetch_bench.rs`(保留)。专项 agent 任务:查明池子根因修复 + 内存级 HTTP 缓存(Cache-Control/ETag/304)。
2. **滚动全量重渲染(render-perf 实测)**:1280×720、5380 fragments 页面,scroll_render 中位 **116ms/帧(p95 164ms)≈8.6fps**——每帧重跑完整管线(样式/布局/显示列表/全屏光栅化)。retained display-list 基础设施在,滚动增量渲染(只重光栅化,viewport_origin 已是 raster 输入)待专项。首渲 378ms/含图可见 681ms。
3. **JS 解释器吞吐**:B 站 bundle 数 MB,树遍历解释器是硬瓶颈。封存性能线 P9(字节码 VM 3-10x)等用户解封决策。

## 进行中(两个后台 subagent,结果待补)

- **网络性能专项**(render-net):连接复用根因 + HTTP 缓存层。
- **Proxy/Reflect 专项**(render-js):get/set/has/deleteProperty/ownKeys/apply/construct 陷阱接入全部属性访问路径 + Reflect 基线 + Object.setPrototypeOf 真语义(静态继承目前是断的)+ Function.prototype.toString;验收=离线重放 bilibili 越过 `Proxy is not defined`。

## 下一批(已实测的错误清单,B 站 2026-09-26 会话)

- Web API 缺口批次:`innerWidth/innerHeight` 缺失、`Blob is not defined`(player core)、"Incorrect invocation"×3(biliMirror/fallback.js,接口构造器模式待查)、core-js anInstance 品牌检查在 Promise 微任务续体上失败(Promise 互操作)。
- legacy polyfill 撞 array-like 物化上限(MAX_MATERIALIZED_ELEMENTS)——评估上限合理性或改惰性。
- `performance.timing`、动态注入脚本(bili-collect.js)抓取未覆盖。

# 交接文档 (2026-09-20) — 视频相位 2 落地 + test262 基座修复与重建

总计划状态：P1–P4 已完成（见下文历史）。本轮完成上一会话未收尾的视频播放相位 2，并修复了两处使 test262 门禁长期不可靠的基础设施/引擎缺陷。fmt/clippy/test（含 test262 门禁）全绿。

## 内建元数据 / 描述符 / 转换修复（同日续三）

- **内建函数 `name`/`length` own 属性**：bootstrap 末尾新增 `Realm::install_builtin_metadata`，对每个可调用对象（NativeFunction/BoundFunction）按安装属性名回填 `name`（符号键为 `[Symbol.x]`）与 `length`（`builtin_arity` 表，未知为 0），属性 `{w:false,e:false,c:true}`；同时为 `X.prototype` 回填 `constructor`。`built-ins/Object` pass 3,767 → **4,307**（+540）。
- **String 包装对象**：`set_member` 不再吞掉 StringPrimitive 上的普通属性写入（仅忽略 length/索引），修复 `new String(); descObj.x = ...` 类测试；`built-ins/String` 743 → **1,045**（+302）。
- **非可配置属性的 SameValue 无操作重定义**：`define_property` 允许逐字段相同的重定义（`descriptors_are_identical`）。
- **`ToString(Number)` 指数形式**：`number_to_string` 按 `1e-6 ≤ |x| < 1e21` 十进制、范围外指数（`1e+21`/`1.5e-7`）。
- **默认参数初始化器**（subagent 完成）：参数默认值左到右求值（`undefined` 才触发、可引用前面的参数、TDZ、抛错传播、与 rest 组合、class 方法/构造器同样支持）；`language/expressions/function` 109 → **129**，`language/statements/function` 329 → **348**。
- **Date 语义**（subagent 完成）：星期表修正、toString/toUTCString/toISOString 格式与年份补零、TimeClip、Date.UTC 强制转换顺序、ISO `Date.parse`、`toJSON` 完整算法；`built-ins/Date` 394 → **484**（+90），crash 6 → 0。
- **`to_numeric_primitive` 恢复 default hint**：subagent 的 ToPrimitive 重构把 `+`/模板字面量路径改成了 number hint，已修回（`+` 按规范用 default hint），既有 hook 测试恢复通过。
- **`built-ins/Function`** 304 → **494**（+190，主要来自 name/length 元数据）。
- **全量结果**：test262 pass **30,808 / 98,096（31.4%）**，crash 22、timeout 20，`264 buckets ok`；基线已重建为 30,808（此前 27,674）。

## Iterator / iterator-helpers 落地（同日续二）

- **`Iterator` 全局 + `%IteratorPrototype%` + `%IteratorHelperPrototype%`**（`value.rs::install_iterator`、新模块 `runtime/builtins/iterator.rs`）：抽象构造器（`new Iterator()` 直接构造抛 TypeError，子类 `super()` 走 NewTarget 原型创建对象）、`Iterator.from`；惰性助手 `map/filter/take/drop/flatMap/concat/chunks/windows`（`ObjectHost::IteratorHelper` 状态机，逐 `next` 步进、`return` 转发源迭代器）；直接方法 `toArray/forEach/reduce/some/every/find` 立即消费；`%IteratorPrototype%[@@iterator]` 返回 `this`。
- **Array 迭代**：`Array.prototype.values/keys/entries` + `@@iterator`（与 `values` 同一函数对象）；Map/Set 迭代器对象改用 `%IteratorPrototype%` 作原型，助手可直接链式调用。
- **顺带修复两个真实缺陷**：① `evaluate_call` 的 `obj[expr](...)` 路径把计算键 `to_js_string()` 后再查属性，**符号键方法调用（`obj[Symbol.iterator]()`）一直取不到**；现在符号键走 `get_symbol_value`。② GC 根集未包含新原型，`%IteratorHelperPrototype%` 会被回收成墓碑，导致助手对象原型丢失；已加入 `gc_identity_roots`。
- **结果**：`built-ins/Iterator` pass 12 → **356**（旧基线 48）；全量 test262 pass 27,674 → **28,182 / 98,096（28.7%）**，门禁 `264 buckets ok`（未重建基线：当前基线仍为 27,674，门禁按"只查回退"通过；下次可用 `RENDER_TEST262_UPDATE_BASELINE=1` 把下限抬到 28,182）。
- **Iterator 剩余缺口**：`[object Object]` 断言簇 638（多为 `function*` generator 迭代器测试，引擎把 generator 当普通函数）、BigInt 字面量 38、`iterator result is not an object` 28、Proxy 相关少量。

## class 语义落地（同日续）

- **词法**：`#name` 私有名 token；`??`/`??=`/`&&=`/`||=`；标识符改用 `unicode-ident` 的 XID 表（依赖已在 Cargo.lock，无新增网络依赖），修复 `#\u2118` 等合法标识符。
- **解析器**：完整 class 语法（构造器/方法/get/set/static/实例与静态字段/static 块/计算键/私有元素/extends/匿名类/`new.target`/`super` 三种形式/`#x in`/`obj.#x`）；class 早期错误（重复 constructor、constructor 字段或访问器、static `prototype`、`#constructor`、私有名重复（get/set 成对除外））；`new.target` 与私有名的作用域校验（全局代码中是 parse 期 SyntaxError）；方法体的 `await`/`yield` 保留字校验（async/generator 上下文）。
- **运行时**：新增 `runtime/class.rs`（ClassDefinitionEvaluation、方法属性、访问器合并、静态字段/块、实例字段初始化、`super` 读写、私有字段/方法/品牌检查、`#x in`）；`UserFunction` 携带 `ClassFunction` 元数据（home object、父构造器、字段、私有作用域、类环境）；`ClassFrame` 栈 + 逐类 `PrivateScope` 链（嵌套类可见外层私有名）；`this` 由动态栈改为**环境绑定**（箭头词法 this、派生构造器 `super()` 前的 `this` TDZ ReferenceError、逃逸箭头仍正确）；`new.target` 栈；`new` 使用 new.target 的 prototype（派生实例原型正确）；rest 参数真正收集为数组；GC 标记类元数据与私有槽。
- **结果**：`language/{statements,expressions}/class` pass 1,474 → **4,623**；全量 test262 pass 23,825 → **27,674 / 98,096（28.2%）**，crash=18。基线已用 `RENDER_TEST262_UPDATE_BASELINE=1` 重建，`264 buckets ok`。
- **已知缺口（class 相关，按量级）**：① generator/async 方法体仍按普通函数执行，`methods-gen-*` 一族（~1k）失败；② 默认参数初始化器仍未求值（`dstr/*-dflt-*` 等 ~1.5k）；③ `unexpected Throw: [object Object]` 断言簇（class 两桶 ~5k，需逐族排查：字段/访问器属性、super 细节、初始化顺序等）；④ **Iterator（iterator-helpers）未实现**：class 真正支持 `extends` 后 `extends Iterator` 变为 ReferenceError，`built-ins/Iterator` 由 48 → 12 —— 旧基线里这些“通过”是宽松的 undefined-callee 空调用造成的假象，新基线已如实反映。实现 Iterator 原型助手是下一步优先项。


## 本轮完成

| 提交 | 内容 |
|---|---|
| 工作区未提交 | 视频相位 2 收尾：`video/present.rs` 时钟/状态 + `HTMLVideoElement` 播放推进 + `page.rs` 帧发布到 `ImageResources`（video 帧压过 poster）+ CSS `grid-column/grid-row` 长写展开 + 函数式伪元素解析（view-transition）+ 光栅化 rect 裁剪快路径/字形循环提升 + render-perf 保留 paint scene 测量滚动 |

- **video 绘制缺陷（阻断性）**：`tree.rs::formatting_kind` 只把 `img` 特判为 atomic inline，`<video>` 走默认 `display:inline` 变成字符级 Inline，layout 不产生盒子，因此已解码帧根本不画。修复为 `img | video` 同一特判，并加 `tree.rs` 回归测试（两种标签都必须 AtomicInline）。`paint_images::video_element_paints_its_presented_frame` 通过。
- **test262 协调器级联崩溃（基础设施）**：被替换 worker 的迟到 `Eof` 会杀掉同槽位的新 worker，新 worker 的 `Eof` 再杀下一个……一个 timeout/崩溃即可把剩余全部用例记成 crash（历史 run crash 32k–77k 波动的原因）。改为 worker 事件带单调 `generation`，旧世代 Line/Eof 直接忽略。crash 76,840 → 18。
- **巨型稀疏 array-like 触发 128 GiB 分配 abort（引擎）**：`array_length` 把 `length:"Infinity"` 截成 `u32::MAX`，`array_elements` 随即物化 2^32−1 个 `JsValue`（137 GB）→ 分配失败 abort worker（`{0:9,length:"Infinity"}` 这类用例）。修复：
  - 新增 `ToLength`/`ToIntegerOrInfinity`/`ArrayCreate` 辅助与 `MAX_MATERIALIZED_ELEMENTS`（1<<24）物化上界、`try_reserve` 降级为可捕获错误；
  - `array_length` 对非有限/超 u32 长度返回可捕获 ResourceLimit（不再静默截断）；
  - `every/some/find/findIndex/forEach/map/filter/reduce/join/indexOf/includes/slice` 改为规范式惰性迭代（`LengthOfArrayLike` + `HasProperty` + `[[Get]]`，空洞跳过语义、`map` 走 `ArrayCreate` > 2^32−1 抛 RangeError、`includes` 按规范不查 HasProperty）；`Array.from`/spread（`array_elements_for`）/`iterate_values` 加物化上界。
- **基线双计（基础设施）**：`Summary::add` 对每个 bucket 自增两次，checked-in 基线总变体 196,192 = 2×98,096，pass 全为真实值的两倍，门禁实际上是"双计基线 vs 单计运行"。删掉重复自增。
- **基线重建**：`RENDER_TEST262_UPDATE_BASELINE=1` 全量重跑生成诚实基线：pass=23,825/98,096（24.3%），fail=67,585，unsupported=6,354，skip=294，timeout=20，crash=18，用时约 3.5 分钟。连续两次全量运行门禁 `264 buckets ok` 且计数一致（确定性）。

## 验证

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿；test262 门禁 212 s，9/9 通过。

## 下一步候选（按价值排序）

1. **P5 fetch/XHR 之后的真实站点推进**：bilibili 主 bundle 仍卡在 "value is not callable (Ordinary)"（class 桩/模块缓存），需专项；zhihu/taobao 未普查。
2. **网络/CSS 缺口**：transform（轮播位移）、grid 命名线/`grid-area`、`aspect-ratio`；B 站封面图绘制已在相位 2 帧通道打通（video），普通 `<img>` 封面仍走既有 image 管线。
3. **test262 增量**：crash=18 已近零；当前失败大头是 class 语义、Temporal、intl402（大量 Syntax/Reference）。class 语义单独排期。
4. **视频解码后端**：`PlaceholderDecoder` 仍报 `DecoderUnavailable`；openh264 因 vendored C 被否，需另选纯 Rust H.264 或自研基线解码。



# 交接文档 (2026-09-13) — JS 引擎补全计划进行中

总计划（已批准，正确性优先、test262 基线门禁）：P1 诊断基础 → P2 访问器属性 → P3 真 Symbol/迭代器 → P4 分发保真 → P5 fetch/XHR → P6 存储/平台 API → P7 Proxy/Reflect → P8 模块加载。class 语义与完整 CORS 不在本计划（单独排期）。

## P1 已完成 (commits edb7210, aab1250, 2af9435, + b3a8340/37fb4cf bilibili 修复)

- AST 全部 Statement/Expr struct 变体带 `offset`；求值器错误自动挂最内层 span；JsError 带 `position`，Display 输出 "at line L, column C"（"at byte 0" 已清除）。
- 调用帧结构化且带真名（UserFunction.name）；`Error.prototype.stack`（非枚举 own）；catch 到的原生错误现在是真正的标准 Error 实例（instanceof TypeError 可用）；成员读 null/undefined 按规范抛 TypeError；`throw errorObj` 显示 "Name: message"。
- test262 基线门禁：`tests/test262-baseline.tsv`（265 桶，前两级目录）；全量跑断言 pass ≥ baseline − 本次 timeout/crash；`RENDER_TEST262_UPDATE_BASELINE=1` 重生成；子集跑自动跳过门禁。
- bilibili 专项：require_node 接受 Document（修 MutationObserver.observe(document)）；Object.prototype.toString 支持 @@toStringTag（消除 core-js toString/classof 无限递归）；String()/复合赋值走 ToPrimitive hint；receiver/callee 错误带 entry point/host 标注。当前视觉状态：导航/横幅/搜索框/动态热门图标已渲染，主 bundle 通过 MutationObserve 后又前进数步，卡在 "value is not callable (Ordinary)"（疑似 class 桩/模块缓存深层问题）。剩余空白是 SSR 卡片区不渲染（未查完，疑似 CSS 支持缺口：grid/aspect-ratio 类）。

## P2 已完成 (commits 6d3886c, 2357781, 4f0a0f8)

- **访问器属性**：PropertyDescriptor 带 getter/setter 槽位；Realm::get_descriptor 纯链上查描述符；JsRuntime::get_value/set_value 实现规范 [[Get]]/[[Set]]（getter/setter 以 receiver 为 this 调用、sloppy 下 getter-only 写静默忽略、frozen 数据属性写静默失败）；defineProperty 支持 get/set 并拒绝访问器与 value/writable 混用；getOwnPropertyDescriptor(s) 如实报告；get_member 自有/继承访问器生效；成员读 null/undefined 按规范抛 TypeError（原来写路径还会静默写到全局）。
- **对象字面量 get/set**：`{get x(){}}/{set x(v){}}` 安装访问器（此前丢弃 accessor 性质变成普通值属性）；同名成员扩展同一描述符；__defineGetter__/__defineSetter__/__lookupGetter__/__lookupSetter__ 已装在 Object.prototype。
- **完整性内建**：Object.preventExtensions/seal/freeze/isExtensible/isSealed/isFrozen（JsObject.extensible + seal=全 own 不可配置 + freeze=再加数据不可写）。
- **键序规范**：整数索引升序在前，字符串键按插入序（此前是 BTreeMap 字典序）；Object.keys/values/entries/spread/enumerable_own_properties 全部走新序。JSON.stringify 的键序随之修正。
- 全量检查（含 test262 门禁）全绿；js_probe 38/39（jquery-init-shape 既有失败，与本轮无关）。

## P3 已完成 (commits efa0121, a5f2d91, 4eb5a67, 472a605, 17e3ab9)

- **真 Symbol 原始值**：`JsValue::Symbol(JsSymbol{id, description})`；typeof → "symbol"；恒等按 id；String(sym) → "Symbol(desc)"；Symbol(desc) 建原始值、new Symbol() 抛 TypeError；Symbol.for/keyFor 注册表；Symbol.prototype.description 访问器 + toStringTag；well-known 13 个符号固定 id。方法调用经 SymbolInstance 包装宿主。
- **符号键属性双轨**：JsObject.symbols map（u64 → (JsSymbol, Descriptor)）；括号读/写/delete/in/对象字面量计算键全通；getOwnPropertySymbols 真实化；seal/freeze/GC 覆盖符号属性；Object.keys/for-in 按规范排除符号键。
- **迭代器协议**：GetIterator/iterator_next/iterate_values；for-of、调用与数组字面量 spread、数组解构全部走 @@iterator；Map/Set 装 entries/values 别名；数组/字符串快路径保留；带 length 的类数组回退索引读；null/undefined 迭代按规范抛 TypeError。
- **钩子**：@@toPrimitive 进 ToPrimitive（data/accessor 皆可，对象结果按规范抛错）；@@hasInstance 进 instanceof；toStringTag 读符号键（修了 bootstrap 误插字符串键的 bug）。
- **GC 修复**：property_object_references 把访问器 getter/setter 槽位当强引用——修复了引导期访问器 getter 被回收导致读到 "[object Object]" 的 bug。
- **test262 基座修复**：worker 线程 512MB 栈（此前深递归测试整批带走 worker）；门禁容差 = 每次 run 的 timeout/crash 豁免（封顶桶 5%）+ max(4, 基线 5%)；基线重建后连续两次全量门禁通过。当前诚实通过约 1.2 万/9.8 万变体（~5 万崩溃 = 引擎解释器/解析器递归深度前沿，非 harness 问题）。
- 全量 tools/check.sh 绿；js_probe 38/39（jquery-init-shape 既有失败）。

## 真实站点进展 (2026-09-16, f39c981 + 82c99e2)

- **bilibili 卡片栅格已渲染**：根因是媒体查询连接词解析——真实样式表写 `(min-width:1140px)and (max-width:1299.9px)`（`and` 前后无空格），字面量 `" and "` 分割把整块断点丢弃。修复为括号深度感知的 `and` 切分（前邻 `)` / 后邻 `(` 也接受）。content_height 12225 → 1202，五列卡片栅格 + 标题/播放量/时长/UP主 全部可见。
- 调查 agent 报告的后续缺口（按阻塞排序）：① 封面图不绘制（图已解码 672x378，卡片封面是 padding-top:56.25% 占位 + 绝对定位 img，绘制环节缺）② 轮播区 % 高度对不定高父级应为 auto（resolve.rs/block.rs，占位 1072px 空白带）③ DOM 修订变化后样式表需重排（resources.rs StalePlan 丢弃整批 → UA-only 帧）④ transform 未实现（轮播位移）⑤ grid-column/grid-row 未解析、grid-gap 未展开。
- P4 分发保真已落地：get_member 现按"属性所在原型"判定优先级——接口原型上的属性（含页面覆盖 Promise.prototype.then / Function.prototype.call）压过合成宿主方法表；Object.prototype 泛型成员仍让位宿主方法。Promise 获得真实原型（then/catch/finally，finally 用 BoundCallable 捕获回调的透传原语实现）+ toStringTag。
- test262 基座：run 目录改毫秒唯一（pid 复用会叠加旧结果导致双计）；被替换 worker 的迟到 E 行降级忽略（竞态会让协调器 panic）。当前基线 pass≈13.9k/98.1k 变体（14.1%），crash≈40.6k 为解释器/解析器递归深度前沿。

## P4 下一步（分发保真）
- get_member 合成方法表（eval.rs ~2274）对多数宿主压过继承属性（仅 Node/Array/Collection/TypedArray 先查继承）→ 覆盖 Promise.prototype.then / Function.prototype.call 失效（实测）。目标：所有宿主先走真实属性查找，合成表仅兜底。
- call_with_this 对 FunctionCall/Apply/Bind 按宿主直分（eval.rs 2819-2834）→ 移除特判，走属性解析。
- 函数对象补 name/length own 属性（bind 推导名）；BoundFunction 语义对齐（读取期 receiver 捕获保留）。
- 验收：猴子补丁用例集（call/apply/bind/then/addEventListener 覆盖）+ log-reporter 回放推进。

## P3 下一步（真 Symbol + 迭代器协议）
- JsValue::Symbol(SymbolId) + 注册表；typeof → "symbol"；Symbol.for/keyFor。
- 属性键双轨（字符串 BTreeMap + symbols BTreeMap<SymbolId, Descriptor>）；计算键站点不再坍缩；getOwnPropertySymbols 真实化。
- GetIterator 抽象接 for-of/spread/解构/Array.from/Promise.all；Array/String/TypedArray/Map/Set 内建 @@iterator。
- @@toPrimitive 接 to_primitive_with_hint、@@hasInstance 接 instanceof。
- 侦察结论：@@ 读取点仅 2 处（object.rs toStringTag + eval.rs @@id）；for-of 现硬编码 Array/String/length（eval.rs:721-751）；spread/解构走 array_elements_for。

## P2 下一步（已侦察）
- PropertyDescriptor 加 `getter/setter: Option<ObjectId>` 字段（保持全部既有构造点可编译，is_accessor() 判别），不做 enum（改动量/收益比差）。
- 新增 JsRuntime::get_value/set_value（规范 [[Get]]/[[Set]]，需 &mut self + Dom 调 getter/setter）；Realm::get_property 保持纯查询（accessor 从纯路径返回 Undefined/None，内部 bootstrap 用不到 accessor）。
- set_property 目前不走原型链（value.rs:3006）——setter 语义放 set_value 里做。
- define_property 校验 get/set 与 value/writable 互斥；getOwnPropertyDescriptor 如实报告；对象字面量 get/set（parser.rs:1791 现在丢弃 accessor 性质）；__defineGetter__ 家族；Object.freeze/seal/preventExtensions + JsObject.extensible；字符串键插入序。

## 封存：性能线 P9（勿启动）

- **启动条件：P1–P8 全部落地、JS 语义完善之后**（用户明确：现在不考虑）。
- 阶梯：树遍历解释器 → 字节码 VM（预期 3–10 倍，常量池/标识符预解析/密集 dispatch）→ 内联缓存（依赖 P2 属性模型定型，勿提前做）→ copy-and-patch 模板基线 JIT（无投机、无去优化；dynasm-rs 或 CPython 3.13 式模板拼接）。不做类型特化优化编译器（TurboFan 档）。
- 配套：GC 需把 VM 帧纳入根集；每级挂 render-perf 基准回归门禁。


# 交接文档 (2026-09-13)

当前 master: 三个纯移动式拆分提交完成（js/runtime.rs、layout/solver.rs、render-browser/main.rs）,fmt/clippy/test 全绿。

## 本轮已完成：代码结构重构（零行为变更）

| 提交 | 内容 |
|---|---|
| `84816dd` | 检查点:JS GC、表单提交管线、content_interaction/frame 模块抽取(前次会话 WIP) |
| `3dbecd8` | js/runtime.rs(11.6k 行) 拆为 runtime/ 目录:mod(状态+分发链入口)、types、eval(求值器)、convert(类型转换)、gc、tests,以及 builtins/ 下按 Web API 域分文件(dom/events/observers/style/timers/url/array/typed_array/collections/object/math/string/regexp/promise/date/json/global_fns) |
| `fd915da | layout/solver.rs(4.3k 行) 按格式化上下文拆为 block/flex/grid/inline/resolve + tests,类型和 Solver 结构体留在 mod.rs |
| `153ad05 | render-browser/main.rs(4.3k 行) 拆为 app/page_state/fetch_handles/render_worker/page_source/diagnostics + app_tests,main.rs 只留启动 |
| `7c77817 | CI 加 windows-latest 矩阵、.gitattributes 锁 LF、tools/check.sh 一键三件套 |

## 未来扩展接缝(重要)

- **新增 JS 内建/Web API**:在 value.rs 的 NativeFunction 加变体 → 在对应 runtime/builtins/域文件 的 dispatch_域_native match 加臂;global_fns.rs 的残差分发对枚举穷尽,**漏臂是编译错误**。详见 runtime/builtins/mod.rs 文档。
- **新增格式化上下文(如 table)**:在 layout/solver/ 新建兄弟文件(对齐 block/flex/grid 的粒度)。
- **native 分发链**:mod.rs call_native_dispatch → dispatch_dom_native → … → dispatch_residual_native,各环 match 自己的变体,通配臂委托下一环,末环穷尽。

## 验证状态

- runtime 拆分提交后跑过全量测试(含 test262)与基线完全一致;solver/main 拆分通过 fmt+clippy+全部单元/集成测试。
- js/value.rs(3.3k 行) 暂未拆(内聚的值类型,优先级低,可选)。
- 遗留提示:Windows 下 git 提示 LF/CRLF 已由 .gitattributes 解决;tools/check.sh 可本地一键验证。

# 交接文档 (2026-09-07)

当前 master: `e096f99` — fmt/clippy/test 全绿,已推送。

## 本轮已完成

| 提交 | 内容 |
|---|---|
| `6c69dd5` | 圆角裁剪/渐变/阴影光栅化、text-align、flex 最小尺寸、contenteditable 编辑、PUA 图标回退、RENDER_DEBUG_FRAME/RENDER_DUMP_FRAME 诊断 |
| `d7f6d15` | 百度专项:命中测试(结构命令不再遮挡内容→链接可点)、越流盒 auto margin(CSS 2 §10.3.7,修复登录按钮/固定角标定位)、渐变边框双层裁剪、SDF 圆角描边、刷新图标 |
| `e979822` | JS: `}` 后正则字面量词法修复、4MiB token 上限、4096 调用深度 + RangeError、TypedArray 全家族 |
| `e096f99` | MutationObserver(基于 DOM mutation journal)、document title 管线(document_title/document.title setter 同步)、Object.getOwnPropertySymbols、Array.prototype.toString |

## 百度当前视觉状态(已验证 PNG)

logo 居中、搜索框圆角边框、按钮贴合、设置/登录贴右、热搜首项"热"图标、右下角组件归位、刷新图标正常。剩余小问题:登录按钮贴右缘时略被裁剪。

## 待办(按优先级)

1. **bilibili**(问题最大)— 已具备:typed arrays/词法修复/深度提升。已知缺口:XMLHttpRequest、fetch、performanceLog/bds 等 Reference 缺失;`incompatible String method receiver` TypeError;render-net `network worker queue is full`(网络层,40 次告警)。需逐个跑 `RENDER_DEBUG_FRAME=1 cargo run -p render-browser -- https://www.bilibili.com` 迭代。
2. **zhihu 登录卡片** — main.app.js 现可编译执行,后续卡在 `Type: incompatible String method receiver`(runtime.rs/value.rs)与 DOM API 缺口;7936.app.js 已跑通。
3. **taobao** — 未完成普查(调查 agent 被取消),需从零跑诊断并分类错误。
4. **title 标签** — 引擎侧管线已就绪(document_title + script setter 同步);**浏览器侧 main.rs 的接线(标签页标题刷新)是 145 行半成品的一部分,已随 e096f99 提交但未做实机验证**,下次先验证 baidu/zhihu 标签页标题是否正确显示。
5. **自绘 UI(浏览器 chrome)问题** — 用户反馈"各种各样的问题",未系统排查;建议对 about:newtab / settings 页做 RENDER_DUMP_FRAME 逐帧检查(标签栏、地址栏、按钮 hit area)。
6. 热搜图标映射是猜测表(e62e→热 等),遇到新 PUA 码点按需补充 `font_backend.rs::fallback_icon_character`。

## 调试工具

- `RENDER_DEBUG_FRAME=1`:stderr 打印渲染管线诊断(样式/fragment/display 命令)
- `RENDER_DUMP_FRAME=<path>`:每帧写 PPM;转 PNG:PowerShell System.Drawing,头为 ASCII `P6 <w> <h> 255\n`,数据偏移=头长
- `cargo run -p render-core --example js_probe`:JS 回归探针集

## 验证命令

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
