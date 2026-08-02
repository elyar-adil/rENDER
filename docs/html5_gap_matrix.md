# rENDER Web 平台能力矩阵

本文档是引擎实现顺序和验收门槛，不是功能宣传页。状态只能由可重复的测试结果推进：

- `Missing`：没有可用实现；
- `Partial`：存在实现，但规范范围或外部一致性测试仍有已知缺口；
- `Conformant`：固定版本的适用外部测试全部通过，且没有已知偏差。

真实网站只用于阶段性集成和视觉验收。网站问题必须缩减成规范测试或最小 fixture，禁止域名特判。

## 当前基线

| 测试层 | 当前状态 | 下一道门槛 |
| --- | --- | --- |
| Rust 单元/集成测试 | `render-core` 249 项通过，workspace 全量通过 | 每个规范修复必须先有最小回归测试 |
| test262 | 固定 revision 共 98,096 变体，11,628 pass、79,820 fail、6,354 unsupported、294 skip，无 timeout/crash | 按失败簇提升；已通过集合不得回退 |
| WPT | 官方固定 revision 的完整外部 checkout 和静态 reftest 批量 runner 已接入，尚未因网络限制完成 checkout 执行 | 运行 `tools/run-wpt-reftests.py`，再单独接入 testharness/navigation |
| 浏览器视觉对比 | 有 fixture 和 Chromium 对比工具，尚未形成规范分组基线 | 每个布局簇提供参考截图或几何断言 |

test262 的当前 pass 约占全部变体的 8.1%，不能描述为 JavaScript 已完成。WPT runner 建立前，任何 CSS/DOM 子系统也不能标为 `Conformant`。

## P0：首屏正确性地基

| 能力簇 | 状态 | 已有能力 | 主要缺口 | 验收测试 |
| --- | --- | --- | --- | --- |
| CSS Syntax | Partial | 规则、声明、常见 at-rule 的容错解析 | token/escape、复杂函数、错误恢复、完整 at-rule 行为 | WPT `css/css-syntax/` |
| Selectors/Cascade | Partial | 常用选择器、specificity、继承、custom properties 基础 | Selectors 4 伪类/伪元素、层叠层、完整默认值与 CSS-wide keywords | WPT `css/selectors/`、`css/css-cascade/` |
| Values/Units | Partial | 常见长度、百分比、部分 `calc()` | 字体相对单位、viewport 单位、完整 math functions、颜色语法、computed/used value 边界 | WPT `css/css-values/`、`css/css-color/` |
| Fonts/Text/Line boxes | Partial | 文本塑形和基础换行 | 逐节点 `font-size`/`line-height`/font 属性参与测量、baseline、`vertical-align`、空白与断词 | WPT `css/css-fonts/`、`css/css-text/`、CSS2 line box |
| Block/Inline formatting | Partial | 基础 block/inline、float、absolute、inline-block | margin collapse、BFC、intrinsic sizing、shrink-to-fit、复杂 float/clear、replaced inline baseline | WPT `css/CSS2/`、`css/css-display/`、`css/css-sizing/` |
| Flexbox | Partial | 单行 flex、方向、基础 grow/shrink、主轴 auto margin | automatic minimum size、wrap、多行、cross-axis auto margin、`align-self`、definite size 传播 | WPT `css/css-flexbox/` |
| Backgrounds/Borders | Partial | background color、边框、CSS 图片值基础 | 图片资源加载与绘制、层、repeat/position/size/origin/clip、border-radius 裁剪 | WPT `css/backgrounds/` |
| Painting/Stacking | Partial | 基础 display list 和 raster | CSS 绘制顺序、stacking context、`z-index`、opacity、clip、transform | WPT `css/css-position/`、`css/css-transforms/`、CSS2 z-order |
| Replaced elements | Partial | `<img>` 基础加载、固有尺寸和绘制 | `object-fit`/`object-position`、SVG、broken image、表单控件固有尺寸 | WPT `html/rendering/replaced-elements/`、`css/css-images/` |

P0 的实施顺序固定为：值和继承 -> 文本与 intrinsic size -> block/inline -> flex -> background/paint。后续算法依赖前一步的 computed/used value，不能倒序用页面参数补偿。

## P1：可交互页面地基

| 能力簇 | 状态 | 主要缺口 | 验收测试 |
| --- | --- | --- | --- |
| HTML parsing | Partial | 完整 tokenizer 状态、错误树构建、fragment parsing、编码嗅探边界 | html5lib tests、WPT `html/syntax/` |
| DOM Core | Partial | 完整 mutation/query/traversal、attribute reflection、`innerHTML`/`textContent` 语义 | WPT `dom/` |
| Events | Partial | capture/bubble、listener options、默认行为、输入事件 | WPT `dom/events/`、`uievents/` |
| Forms | Partial | 表单控件状态、label、提交、校验、默认样式和交互 | WPT `html/semantics/forms/` |
| Event loop | Partial | task/microtask、Promise jobs、timer nesting、render opportunity | WPT `html/webappapis/scripting/event-loops/` |
| Navigation/History | Partial | `location` 可写属性和导航、history traversal、重定向与 URL Standard 边界 | WPT `html/browsers/browsing-the-web/`、`url/` |
| Fetch/XHR | Missing | 请求对象、Promise/事件联动、CORS、redirect、abort、响应体 | WPT `fetch/`、`xhr/` |
| Canvas/Media | Missing/Partial | Canvas 2D、媒体元素状态机、解码/播放管线 | WPT `html/canvas/`、`html/semantics/embedded-content/media-elements/` |

## P1：ECMAScript

test262 按失败簇推进，而不是按站点脚本逐文件打补丁：

1. lexer/parser 与 early errors；
2. execution context、scope、closure、`this`；
3. property descriptors、prototype、Proxy/Reflect；
4. Array/String/Object/Number/RegExp 等基础 built-ins；
5. iterator/generator、Promise、async jobs；
6. module graph、import/export；
7. typed arrays、ArrayBuffer、DataView；
8. Intl、Temporal 等独立大簇。

每一簇的完成条件是：固定 test262 路径全量运行、适用测试 100% pass、无 crash/timeout、unsupported 数量有明确下降。总目标可以是固定 revision 的适用测试 100%，但在模块、async 和 host 能力仍被分类为 unsupported 时，不得宣称 test262 100%。

## P2：现代应用兼容

- CSS Grid 完整轨道尺寸、隐式网格、subgrid；
- Shadow DOM、Custom Elements、slotting；
- CSSOM、Resize/Intersection/Mutation Observer；
- storage、cookies、URL/Streams/Encoding Web APIs；
- accessibility tree、IME、clipboard、drag and drop；
- cache、service worker、security policy 和多进程隔离。

## 阶段验收

1. **M1 静态文档**：P0 CSS/布局子集有 WPT 基线，background、字体、block/inline、flex 的目标子集无已知失败。
2. **M2 交互文档**：DOM、events、forms、navigation、timer/Promise 目标子集通过，百度搜索流程可用。
3. **M3 应用启动**：fetch/XHR、module、现代 JS 核心簇可运行常见 hydration/bootstrap。
4. **M4 媒体应用**：Canvas/Media、资源调度和播放状态机达到视频站基础播放要求。

百度、hao123、知乎、腾讯新闻和 bilibili 仅在每个里程碑末运行一次回归，失败先归入上表能力簇，再补最小规范测试。
