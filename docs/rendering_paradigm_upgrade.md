# 渲染范式升级方案（Layout + Script Execution）

> 目标：从“局部修补”切换到“规范驱动的渲染流水线”，优先保证布局与 DOM/脚本行为正确性。

## 0. 现状根因（非表象问题）

### A. 布局系统是“按节点递归直排”，不是“格式化上下文驱动”

当前入口在 `layout/__init__.py` 中按 `display` 分发，直接对子节点逐个布局，且绝对定位元素通过二次遍历补算。该结构导致：

1. **BFC/IFC/Flex/Grid 的边界不稳定**：格式化上下文没有统一抽象，算法之间通过共享可变状态拼接，导致边界行为（margin collapse、浮动影响范围、包含块确定）偏差累积。
2. **绝对定位是后处理补丁**：`_layout_deferred_abs()` 在主布局后再跑，无法和正常流共享同一“约束求解阶段”，导致 containing block、静态位置、尺寸依赖关系不完整。
3. **Inline 布局与 Block 布局耦合为“测量即放置”**：缺失浏览器通用的“先构建 formatting structure，再 line box 求解”的分层。

### B. CSS 计算模型把“computed value”和“used value”混在一起

`css/computed.py` 在全树遍历阶段就把大量长度转成 px，并写回 `node.style`。这会破坏规范中的阶段语义：

- `%`、`auto`、`min-content/max-content` 等值应在 **used value/layout 时** 基于上下文求值；
- 当前提前求值会丢失上下文依赖，迫使布局层做大量特判，长期演化成 case-by-case。

### C. Cascade/Inheritance/Var Resolution 顺序与数据结构不可追溯

`css/cascade.py` 直接把结果覆盖到元素样式字典，再做继承和 `var()` 解析。问题：

1. **无法追踪声明来源**（origin / layer / specificity / order）；
2. **无法支持增量重算**（DOM 改动后只能全量覆盖）；
3. **伪元素与主元素样式生命周期混杂**，难以做一致 invalidation。

### D. 脚本执行模型是“脚本串行执行 + 立即 drain microtask”

`engine._execute_scripts()` 和 `js/promise.py` 当前把 microtask 在很多点上“立刻清空”，Promise settle 时还会递归 drain。该模型与 HTML 事件循环差距很大：

1. **缺失 task queue / microtask checkpoint / rendering opportunity** 三段式；
2. `setTimeout` 直接同步回调，不具备 timer task 语义；
3. script、DOM mutation、Promise、XHR/fetch 之间缺少统一调度器，导致执行时序不稳定。

---

## 1. 目标架构（可落地模块级方案）

## 1.1 新渲染管线（阶段化 + 可失效重算）

引入 `RenderPipelineV2`（新模块建议：`engine/pipeline_v2.py`），阶段如下：

1. **DOM 构建阶段**（parser + tree builder）
2. **Style 阶段**
   - selector match
   - cascade（保留来源元数据）
   - computed value（不提前消费上下文相关值）
3. **Layout 阶段**
   - build formatting tree（匿名盒、伪元素盒、run-in 归并等）
   - establish formatting contexts（BFC/IFC/FlexFC/GridFC/TableFC）
   - constraint solving -> used values -> geometry
4. **Paint 阶段**
   - stacking context tree
   - paint order
5. **Compositing（可后续）**

同时引入统一 `InvalidationGraph`：

- style invalidation（类名/属性变化）
- layout invalidation（尺寸/display/position变化）
- paint invalidation（颜色/边框变化）

## 1.2 CSS 引擎重构：从“覆写字典”到“可追踪 style graph”

建议新增：

- `css/style_tree.py`
  - `SpecifiedStyle`：保留声明来源（origin, specificity, order, important, selector_id）
  - `ComputedStyle`：仅完成规范定义的 computed 级求值
  - `UsedStyleResolver`：在 layout context 中解 `%/auto/calc(min/max/clamp)`

关键原则：

- `Element.style` 不再作为最终计算结果容器，而是转为 author-inline 输入。
- 输出独立 `StyleNode`，供 layout 读取，避免“布局回写样式”污染。

## 1.3 布局重构：Formatting Tree + Constraint Pass

建议新增：

- `layout/tree_builder.py`
  - DOM -> Formatting Tree（含 anonymous block/inline box）
- `layout/contexts.py`
  - `BlockFormattingContext`
  - `InlineFormattingContext`
  - `FlexFormattingContext`
  - `GridFormattingContext`
- `layout/solver.py`
  - 两阶段：
    1) intrinsic measurement（min/max content contribution）
    2) final layout（available space 下解 used size/position）

绝对定位不再 deferred 补跑，而是并入 solver：

- 先确定 containing block
- 求静态位置
- 解 inset/auto 约束
- 与 normal flow 统一产出 fragment

## 1.4 脚本执行重构：事件循环内核

新增 `js/event_loop.py`：

- task queues：`script`, `timer`, `network`, `user-interaction`
- microtask queue：Promise reactions, mutation observer delivery
- event loop tick：
  1) 取一个 macrotask 执行
  2) 执行 microtask checkpoint（清空 microtask）
  3) 若有 rendering opportunity 则触发 style/layout/paint flush

并改造：

- `setTimeout` 注册 timer task（最短延迟语义）
- script 执行不再直接 `drain_microtasks()`，而是由 loop checkpoint 统一处理
- fetch/XHR completion 进入 network task queue

## 1.5 DOM 与 Layout 的一致性交互

新增 `dom/mutation.py`（或并入 `js/dom_api.py` 的 mutation 子模块）：

- 每次 DOM 变更生成 mutation record
- 标记 invalidation（style/layout/paint）
- 由 event loop 在恰当时机触发批量 flush

效果：DOM API（append/remove/setAttribute）不再立即强制全量重排，而是遵循浏览器“延迟到渲染机会”模型。

---

## 2. 优先级（必须按顺序推进）

### P0（最高）：事件循环与样式/布局阶段解耦

1. 落地 `js/event_loop.py`，替换当前“脚本后立即 drain”机制。
2. 建立 `StyleNode` 与 `LayoutNode` 数据结构，阻断 `node.style` 直接承载 used value。

> 不做这一步，后续所有 layout 修复都会继续碎片化。

### P1：Formatting Tree + Block/Inline 主干重写

1. 先重写 Block + Inline（覆盖 80% 页面结构）；
2. 绝对定位并入统一 constraint solver；
3. 浮动/margin collapse 迁移到 BFC 内部规则。

### P2：Flex/Grid/Table 接入新内核

将现有 `layout/flex.py`, `grid.py`, `table.py` 适配到新 `LayoutContext` 接口，复用统一测量与 containing block 解析。

### P3：增量失效与性能

1. 引入 subtree-level invalidation；
2. 增量 style recalc / 局部 layout；
3. 按需 paint list rebuild。

---

## 3. 实施蓝图（迭代计划）

### Milestone 1：执行模型基座（1~2 周）

- 新建 event loop 与队列模型；
- Promise/microtask 改为 checkpoint 驱动；
- 让 script/fetch/timer 全部走 task queue。

验收：

- Promise 链、MutationObserver、`setTimeout` 顺序测试通过；
- 现有 JS 测试不回退。

### Milestone 2：Style Graph（1~2 周）

- 引入 `SpecifiedStyle/ComputedStyle/UsedStyleResolver`；
- 把 `css/computed.py` 的“提前 px 化”迁移到 used-value 阶段。

验收：

- 百分比、auto、font-relative 单位在上下文变化下结果正确；
- cascade 可输出来源调试信息（便于对齐 Chrome/WebKit）。

### Milestone 3：Layout 主干切换（2~4 周）

- 上线 formatting tree + block/inline solver；
- 绝对定位移入统一求解。

验收：

- 关键回归页几何 diff 显著下降；
- inline/block/floats/positioning 套件通过率提升。

### Milestone 4：扩展格式化上下文与增量化（持续）

- flex/grid/table 接口迁移；
- invalidation 优化和性能回收。

---

## 4. 风险与控制

1. **重构跨度大**：采用“双轨运行”策略。
   - `pipeline=v1|v2` 开关并存；
   - 每个里程碑先在测试中 A/B 对比。
2. **行为变化范围广**：必须先补“行为测试”再迁移。
   - 重点补：事件循环顺序、包含块解析、百分比 used value。
3. **短期性能回退**：允许。
   - 本轮以正确性优先，性能在 P3 回收。

---

## 5. 与当前代码的映射改造建议

- `engine.py`
  - 抽离 `_pipeline()` 为可插拔 pipeline 接口（v1/v2 并存）
  - `_execute_scripts()` 迁移到 `js/event_loop.py` 驱动
- `css/cascade.py` / `css/computed.py`
  - 保留 parser 与 selector 资产，替换结果承载模型
- `layout/__init__.py` / `layout/block.py` / `layout/inline.py`
  - 逐步迁移到 formatting tree + solver
- `js/promise.py`
  - 移除 settle 时立即 drain；交给 event loop checkpoint

---

## 6. 结论

当前偏差的根因不是“单点算法错误”，而是 **执行模型与布局模型的阶段边界错误**。要收敛到 Chrome/WebKit 级行为一致性，必须先完成：

1. **事件循环规范化**（task/microtask/render checkpoint）
2. **样式阶段分层**（specified/computed/used）
3. **布局范式切换**（formatting tree + constraint solver）

这三者是同一问题链条，不能再拆成零散 patch 处理。
