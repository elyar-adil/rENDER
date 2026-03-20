# qq.com 兼容性分析报告

> 分析日期：2026-03-20
> 目标页面：https://www.qq.com
> 原则：从零实现，correctness over completeness，每一行代码都要有价值

---

## 现状定位

qq.com 的初始 HTML 有**部分静态骨架**（导航、少量新闻条目），但大量内容通过 JS 动态注入。
当前 rENDER 对 qq.com 的渲染：静态 HTML 骨架可见，但布局严重错乱，装饰元素大量缺失。

修复路径按**实现难度/收益比**排序——CSS 层面的修复收益最确定、实现最干净；JS 放最后且要做就做正确。

---

## 优先级排序

### P1 — 低难度，高收益（可以立刻做正确）

#### 1. `box-shadow` / `text-shadow` 渲染

**qq.com 使用**：新闻卡片投影、导航栏底部阴影、弹框阴影。

**为何未实现**：QPainter 原生支持，只是从未连接。

**修复范围**：`rendering/qt_painter.py` 中，`DrawRect` 执行前附加 `painter.setShadow()`；属性值已经由 cascade 正确解析，只需读取。

```python
# css/properties.py 已有：
# 'box-shadow': PropertyDef(initial='none', inherited=False)
# 解析结果形如: [(offset_x, offset_y, blur, spread, color, inset), ...]
# 只需在 qt_painter.py 读取并调用 QPainter
```

**难度**：低。约 30 行。**影响**：卡片层次感全部恢复。

---

#### 2. `:hover` / `:focus` 始终匹配 Bug

**当前行为**：`css/selector.py` 中动态伪类无条件返回 `True`，导致所有 hover 样式常驻显示（qq.com 的导航 hover 背景色会默认出现在所有链接上）。

**修复**：动态伪类默认返回 `False`（不匹配）。hover 状态由 Qt 鼠标事件驱动，触发局部重新 cascade。这样 hover 样式不显示，比样式错误显示更正确。

**难度**：极低。一行修复。**影响**：消除视觉噪音，导航栏恢复正常外观。

---

#### 3. `::before` / `::after` 伪元素渲染

**qq.com 使用**：几乎所有装饰元素——分类标签左侧竖线、箭头图标、数字角标、分隔符。

**实现方式**：
- cascade 阶段：为有 `content` 属性的元素生成虚拟子节点（`PseudoElement`）
- layout 阶段：`PseudoElement` 按 `display` 值（通常 `inline` 或 `block`）正常参与布局
- 无需新的渲染命令，现有 DrawRect/DrawText 即可

```
Element
  ├── ::before  (虚拟节点，content:"▶", color:red)  ← 插入到 children 前
  ├── child1
  └── ::after
```

**难度**：中。约 80 行（cascade 生成节点 + layout 识别）。**影响**：大量装饰元素恢复。

---

### P2 — 中等难度，高收益（做了质变）

#### 4. CSS Grid 布局引擎

**qq.com 使用**：主内容区多列网格（`grid-template-columns: repeat(3, 1fr)`）、新闻卡片排列。当前降级为 block 垂直堆叠，整个页面变成单列。

**最小正确实现**（覆盖 qq.com 实际用法）：

| 需要实现 | 跳过（门户不用） |
|---------|----------------|
| `grid-template-columns`: `px`、`fr`、`repeat(n, x)` | `subgrid` |
| `grid-template-rows`: 同上 | `masonry` |
| `grid-column/row`: `span N`、`start / end` | `grid-template-areas`（可延后） |
| `gap` / `row-gap` / `column-gap` | `auto-fit` / `auto-fill` |
| `align-items` / `justify-items` | `dense` packing |

**难度**：高。约 300 行（新文件 `layout/grid.py`）。**影响**：整个页面布局从单列变为正确的多列网格。

---

#### 5. `position: sticky`

**qq.com 使用**：顶部导航栏 `position:sticky; top:0`，滚动时固定。

**实现**：layout 阶段按 `static` 计算初始位置；render 阶段根据滚动偏移判断是否钳制到 `top` 值。不需要修改 layout 数据结构，只需在 `qt_painter.py` 绘制时偏移。

**难度**：中。约 50 行。**影响**：导航栏不再随内容滚走。

---

#### 6. `transform` 基础应用

**qq.com 使用**：`translateX/Y`（定位修正）、`rotate`（箭头翻转）、`scale`（hover 放大）。

**最小正确实现**：
- `translate(x, y)` / `translateX` / `translateY`：影响 layout 位置
- `rotate(deg)`：绘制时旋转
- `scale(n)`：绘制时缩放

display list 已有 `PushTransform` / `PopTransform`，只需在 layout 末尾填入矩阵。

**难度**：中。约 60 行。**影响**：箭头朝向正确，部分元素定位修正。

---

### P3 — 中等难度，中等收益

#### 7. `@font-face` 字体加载

**qq.com 使用**：腾讯 CDN 自定义中文字体。降级到系统字体在 Linux 上可能无可用中文字体（显示豆腐块）。

**实现**：解析 `@font-face` 块，下载字体文件（`.woff2`/`.ttf`），用 `QFontDatabase.addApplicationFont()` 注册。

**难度**：低-中。约 40 行。**影响**：中文文字正确显示。

---

#### 8. `loading="lazy"` 图片立即加载

**qq.com 使用**：所有新闻缩略图带 `loading="lazy"`，当前被忽略导致图片不加载。

**修复**：解析 HTML 时忽略 `loading` 属性，所有图片都立即加载（rENDER 无滚动懒加载需求）。

**难度**：极低。删除一个 if 判断。**影响**：新闻图片全部出现。

---

#### 9. SVG `<img src=".svg">` 支持

**qq.com 使用**：Logo、图标。PyQt6 内置 `QSvgRenderer`，可用于 `<img>` 标签。

**实现**：`network/http.py` 下载 SVG 文本，`rendering/qt_painter.py` 用 `QSvgRenderer` 绘制。内联 `<svg>` 暂不处理。

**难度**：低。约 30 行。**影响**：Tencent logo 和图标出现。

---

#### 10. `radial-gradient` 支持

**qq.com 使用**：圆形头像边框渐变、按钮背景。

**实现**：`rendering/qt_painter.py` 用 `QRadialGradient` 替代纯色填充。CSS 解析已识别 `radial-gradient()`，只需渲染端实现。

**难度**：低。约 20 行。**影响**：按钮和头像视觉效果恢复。

---

### P4 — JavaScript（诚实评估）

#### 从零实现 JS 的真实成本

"从零正确实现"JS 是量级上与整个 rENDER 现有代码相当的工程。关键难点：

| 问题 | 难度 |
|------|------|
| Lexer（含正则字面量、模板字符串、自动分号插入） | 中 |
| Parser（运算符优先级、解构、展开、箭头函数） | 高 |
| 闭包和作用域链 | 高 |
| `this` 绑定规则 | 高 |
| 原型链 | 高 |
| 异步（Promise、async/await、事件循环） | 极高 |

qq.com 的 inline 脚本（从 hao123 同类分析）**立即**用到了：闭包、正则、`try/catch`、`for...in`、字符串 `.replace(/regex/, fn)`。没有捷径——要跑起来就要基本正确。

#### 正确的边界：只实现"数据岛"模式

qq.com / hao123 的一个关键模式：**把 JSON 数据嵌在 HTML 里，JS 读取后渲染**：

```html
<script class="head-links" type="application/json">
  [{"title":"新闻","url":"https://news.qq.com"},...]
</script>
```

```javascript
var data = JSON.parse(document.querySelector('.head-links').textContent);
element.innerHTML = data.map(item => `<a href="${item.url}">${item.title}</a>`).join('');
```

**可以不实现 JS 引擎，而是直接识别这个模式**：
- HTML parser 阶段：识别 `<script type="application/json">` 或 `type="text/json"`，把内容作为数据节点保留在 DOM
- 渲染阶段：静态展示这些数据节点的内容（不执行 JS，但数据可见）

这不是 JS 引擎，是 HTML 层面的特殊处理，约 20 行。

#### 如果要实现最小 JS 解释器

按"correctness over completeness"，最小正确子集：

**Phase JS-1（约 600 行，可独立交付）**：
- Lexer：数字/字符串/标识符/关键词/运算符
- Parser → AST：`var/let/const`、函数声明和调用、对象/数组字面量、`if/else`、`for`、`return`、`try/catch`、属性访问
- 解释器：基本表达式求值、全局作用域、`JSON`（内置）、`console.log`（打印）
- DOM binding：`document.querySelector`、`element.innerHTML`（setter 调用 HTML parser）

**不包含**（跳过直到 Phase JS-2）：
- 闭包、原型链、`this`、正则、`async/await`、`Promise`、XHR

**Phase JS-1 的实际效果**：能跑不依赖闭包的简单初始化脚本（约占 inline JS 的 30-40%）。

**结论**：JS 实现价值存在，但在 CSS P1/P2 全部完成前不值得启动。CSS 修复对已有静态 HTML 内容的改善是确定的；JS 收益依赖脚本复杂度，风险高。

---

## 汇总：按优先级的实现路线

```
P1（约 2 天，效果立竿见影）
  ├── :hover 始终匹配 Bug 修复          ~1行
  ├── box-shadow / text-shadow 渲染     ~30行
  ├── loading="lazy" 忽略              ~1行
  └── ::before / ::after 渲染          ~80行

P2（约 1 周，布局质变）
  ├── CSS Grid 基础实现                ~300行
  ├── position: sticky                 ~50行
  └── CSS transform 基础               ~60行

P3（约 3 天，视觉完善）
  ├── @font-face 字体加载              ~40行
  ├── SVG <img> 支持                   ~30行
  └── radial-gradient                  ~20行

P4（长期，按需启动）
  └── JS 解释器 Phase JS-1             ~600行
      （前提：P1+P2+P3 全部完成）
```

---

## 已实现但有已知 Bug

| Bug | 位置 | 影响 |
|-----|------|------|
| `:hover/:focus/:active` 始终匹配 | `css/selector.py` | 导航栏 hover 样式默认显示 |
| `border-radius` 两值语法（椭圆）未处理 | `css/computed.py` | 圆角变形 |
| z-index stacking context 不完整 | `layout/block.py` | 下拉菜单层级错误 |
| margin collapsing 不完整 | `layout/block.py` | 段落间距偏差 |
| flex-basis: 0 与 auto 语义差异 | `layout/flex.py` | 局部宽度计算偏差 |
