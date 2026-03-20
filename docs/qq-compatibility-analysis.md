# qq.com 兼容性分析报告

> 分析日期：2026-03-20
> 目标页面：https://www.qq.com（腾讯门户）
> 说明：由于网络限制无法直接抓取，基于 qq.com 已知技术栈 + rENDER 功能清单进行分析

---

## 总体结论

qq.com 是一个**重度依赖 JavaScript 动态渲染**的门户网站，静态 HTML 只是一个空壳，绝大多数内容通过 JS 注入。当前 rENDER 对 qq.com 的渲染结果接近**白页**，主要障碍是 JavaScript 完全未实现。即使解决 JS 问题，CSS 层面也有若干严重缺失。

---

## 按重要性排序的 Feature 清单

### 🔴 极严重 — 导致页面内容完全丢失

| # | Feature | qq.com 使用情况 | rENDER 状态 | 影响 |
|---|---------|----------------|-------------|------|
| 1 | **JavaScript 执行引擎** | 页面内容（新闻列表、导航状态、广告位）全部由 JS 动态注入；初始 HTML 仅有容器 div | ❌ 完全未实现（Phase 5 存根） | 页面内容约 90% 丢失，只剩静态 HTML 骨架 |
| 2 | **Fetch / XHR（AJAX）** | 新闻内容、推荐位通过 API 异步加载 | ❌ 未实现（依赖 JS） | 动态内容全部缺失 |
| 3 | **document.write / innerHTML** | 用于渲染初始内容模板 | ❌ 未实现 | 即使有 JS 引擎，DOM 操作 API 也需单独实现 |

---

### 🔴 极严重 — 导致布局完全错乱

| # | Feature | qq.com 使用情况 | rENDER 状态 | 影响 |
|---|---------|----------------|-------------|------|
| 4 | **CSS Grid 布局** | 主内容区域（新闻卡片网格、多列布局）大量使用 `display:grid`、`grid-template-columns`、`grid-area` | ❌ 属性已注册但 layout 引擎未实现 | 网格布局降级为 block 堆叠，列结构完全丢失 |
| 5 | **position: sticky** | 顶部导航栏使用 `position:sticky` 固定滚动 | ❌ 未实现（只有 static/relative/absolute/fixed） | 导航栏随页面滚走，交互体验破坏 |
| 6 | **CSS 自定义属性（变量）级联** | 几乎所有颜色、间距通过 `--qq-primary-color`、`--spacing-*` 等变量定义 | ⚠️ 已实现但可能有 fallback 解析 bug | 若变量解析失败，整个主题色彩丢失 |

---

### 🟠 严重 — 导致大量视觉元素消失

| # | Feature | qq.com 使用情况 | rENDER 状态 | 影响 |
|---|---------|----------------|-------------|------|
| 7 | **::before / ::after 伪元素渲染** | 图标、装饰线、箭头、数字角标等大量使用伪元素 | ❌ 已解析但完全不渲染 | 几乎所有 UI 装饰元素消失，导航箭头、分类标签丢失 |
| 8 | **SVG（内联 + 外链）** | Logo、菜单图标、社交图标均为 SVG（内联 `<svg>` 或 `<img src=".svg">`） | ❌ 完全未支持 | Tencent logo 及所有图标变为空白 |
| 9 | **CSS transform** | 下拉菜单展开（`translateY`）、hover 缩放（`scale`）、箭头旋转（`rotate`） | ❌ 属性已注册但未应用到渲染 | 动画效果全部失效，部分布局偏移元素位置错误 |
| 10 | **CSS animation / transition** | 新闻 ticker 滚动（`@keyframes`）、tab 切换淡入、hover 颜色渐变 | ❌ 已注册但不执行 | 所有动画静止，marquee 效果丢失 |
| 11 | **box-shadow / text-shadow** | 卡片投影、模态框阴影、文字浮雕效果 | ❌ 已注册但不渲染 | 卡片没有层次感，扁平化视觉效果丢失 |

---

### 🟡 较重要 — 显著影响视觉质量

| # | Feature | qq.com 使用情况 | rENDER 状态 | 影响 |
|---|---------|----------------|-------------|------|
| 12 | **Web 字体（@font-face）** | 引用腾讯 CDN 上的自定义中文字体 | ❌ @font-face 规则被忽略 | 降级为系统字体（Linux 上可能缺中文字体，显示豆腐块） |
| 13 | **WebP 图片格式** | 几乎所有新闻缩略图都是 WebP | ⚠️ 依赖 PIL/Pillow；Pillow 支持 WebP 但需系统 libwebp | 若 libwebp 未安装，图片全部无法显示 |
| 14 | **radial-gradient / conic-gradient** | 按钮、头像边框、loading 动画使用径向/锥形渐变 | ❌ 仅支持 linear-gradient | 这些元素背景丢失或退化为纯色 |
| 15 | **CSS filter（blur, brightness 等）** | 图片悬停模糊、暗色遮罩效果 | ❌ 未实现 | hover 状态下图片效果丢失 |
| 16 | **clip-path** | 部分装饰图形（斜切角、多边形标签）使用 clip-path | ❌ 未实现 | 形状变为普通矩形 |
| 17 | **图片懒加载（loading="lazy" / IntersectionObserver）** | 所有新闻图片使用懒加载 | ❌ 未实现（loading 属性忽略，IntersectionObserver 需 JS） | 未加载图片显示为空（破图 alt 文字） |

---

### 🟡 中等重要 — 局部布局问题

| # | Feature | qq.com 使用情况 | rENDER 状态 | 影响 |
|---|---------|----------------|-------------|------|
| 18 | **Flexbox 边缘 case** | 复杂嵌套 flex 容器，flex-wrap + align-content 组合 | ⚠️ 已实现，但嵌套多层时可能布局错误 | 局部模块宽高计算可能出错 |
| 19 | **margin collapsing（完整规范）** | 相邻 block 的 margin 折叠影响段间距 | ⚠️ 部分实现 | 文章段落间距偏大/偏小 |
| 20 | **overflow:hidden + z-index stacking context** | 下拉菜单需要正确的堆叠上下文才能显示在内容之上 | ⚠️ z-index 已注册但堆叠上下文（stacking context）未完整实现 | 菜单可能被遮挡或显示在错误层级 |
| 21 | **@media 查询（高级条件）** | 响应式布局，`min-width`/`max-width`，`hover: hover`，`prefers-color-scheme` | ⚠️ 基础 width 条件已实现；feature queries 未支持 | 移动端断点可能不触发，prefer 查询被忽略 |
| 22 | **:hover / :focus 伪类（动态）** | 导航菜单展开、链接高亮 | ⚠️ 已识别但始终 match（无事件驱动更新） | 所有 hover 样式常驻显示，无交互变化 |

---

### 🟢 基本已实现 — 但需验证正确性

| # | Feature | qq.com 使用情况 | rENDER 状态 | 潜在 Bug |
|---|---------|----------------|-------------|----------|
| 23 | **CSS 变量（`var()`）** | 大量嵌套使用 | ✅ 已实现（含 fallback） | 嵌套 var 引用循环时可能挂起 |
| 24 | **Flexbox 基础布局** | 导航栏、新闻卡片行 | ✅ 已实现 | flex-basis:0 vs auto 语义可能有差异 |
| 25 | **`display:none` / visibility:hidden** | 折叠内容、隐藏模块 | ✅ 已实现 | 需验证嵌套情况 |
| 26 | **border-radius** | 按钮、头像、卡片圆角 | ✅ 已实现 | 椭圆圆角（两值语法）可能未覆盖 |
| 27 | **rgba / hsla 颜色** | 半透明遮罩、hover 状态 | ✅ 已实现 | 基本正确 |
| 28 | **`<a>` 链接** | 几乎所有内容都是链接 | ✅ 点击可触发导航 | 正常工作 |
| 29 | **`<img>` 加载** | JPEG/PNG 新闻图片 | ✅ 已实现 | HTTP 图片可能需要跟随重定向 |
| 30 | **HTML 语义元素解析** | `<header>`, `<nav>`, `<main>`, `<article>`, `<section>` | ✅ 已解析 | 正确识别为 block 元素 |

---

## 修复优先级建议

基于**渲染改善幅度 / 实现难度**权衡排序：

### Phase A：最高优先级（解锁页面基本内容）

1. **CSS Grid 布局引擎**（难度高，收益最大）
   - `grid-template-columns/rows`, `grid-area`, `grid-column/row` span
   - qq.com 主要布局骨架依赖 Grid

2. **::before / ::after 伪元素渲染**（难度中，视觉收益显著）
   - 在 layout 阶段生成虚拟元素，插入 display list

3. **CSS transform 应用**（难度中）
   - 在 display list 中生成 `PushTransform` 命令（基础矩阵已有框架）

### Phase B：中优先级（改善视觉质量）

4. **box-shadow 渲染**（难度低，收益明显）
   - QPainter 支持，只需在 DrawBorder 后追加阴影绘制

5. **SVG 基础支持**（难度高）
   - 至少支持 `<img src=".svg">` 用 Qt SVG 模块渲染

6. **position: sticky**（难度中）
   - 在滚动时重新计算 sticky 元素位置

7. **@font-face 字体加载**（难度低）
   - 下载字体文件，注册到 Qt 字体系统

8. **WebP 图片确认**（难度极低）
   - 验证 Pillow + libwebp 是否安装，必要时加 fallback

### Phase C：长期（JavaScript 引擎）

9. **JavaScript 引擎**（难度极高，必须实现才能渲染现代网站）
   - 优先实现：DOM API（querySelector, innerHTML, createElement）
   - 其次：Fetch/XHR
   - 最后：事件系统

---

## 已实现但有 Bug 的 Feature 验证清单

运行以下测试可以快速定位已实现功能中的 bug：

```bash
# 1. 验证 CSS 变量级联
python engine.py tests/fixtures/css_vars_nested.html

# 2. 验证 Flexbox 嵌套
python engine.py tests/fixtures/flex_nested.html

# 3. 验证 border-radius 椭圆（两值）
# border-radius: 50% 20px  →  应该是椭圆

# 4. 验证 :hover 伪类不应常驻
# 当前实现中 :hover 始终 match，会导致所有 hover 样式默认显示
```

已知 Bug（从代码审查发现）：
- `:hover`, `:focus`, `:active` 选择器始终匹配（无事件驱动），qq.com 的悬停样式会默认显示
- `position:sticky` 完全缺失，导航栏会随页面流动
- z-index stacking context 未完整实现，下拉菜单层级可能错误
