# Testing Strategy: "Pass Tests => Render Real Pages"

本项目的测试目标不是“语法正确”，而是“通过测试后，现代网页可以被正确渲染，且结果接近主流浏览器”。

## 1. 分层策略

### A. Engine 单元测试（快）
- 覆盖 HTML/CSS/JS 解析、选择器、级联、长度计算、布局子模块。
- 价值：快速定位算法错误，减少调试成本。

### B. 页面渲染契约测试（中）
- 契约定义在 `docs/real_site_acceptance.md`：文档标题、语义结构、可���链接、
  `role=search` 表单、资源分类、600px 视口以下仍继续的块级布局、滚动区内的有序文本块。
- 执行体是独立 crate `tests/real_site_tasks/`（`cargo test --manifest-path
  tests/real_site_tasks/Cargo.toml`），fixture 在 `tests/fixtures/real_sites/`，
  **完全离线**：测试用确定性本地值替换资源响应，不需要联网。
- 引擎层的布局/绘制契约断言在 `crates/render-core/tests/`（`layout_paint_regressions.rs`、
  `layout_positioning.rs`、`paint_images.rs`）和 `crates/render-layout/src/solver/table_tests.rs`。
- 价值：把"看起来差不多"转为可执行契约。

### C. 浏览器差异测试（慢）
- 当前形态是**人工对比**，不是自动化：`RENDER_DUMP_FRAME=<path>` 落 PPM，
  `python tools/ppm2png.py <in.ppm> <out.png>` 转 PNG，再与参考浏览器截图人工比对。
  参考截图与抓取产物在 `.artifacts/` 与 `.diag/`。
- 官方 WPT reftest runner 存在于 `crates/render-core/tests/wpt_reftests.rs`
  （`cargo test -p render-core --test wpt_reftests -- --ignored --nocapture`），
  但**从未真正跑通过一次完整 checkout**，所以本仓库里没有任何数字是 WPT 结果。
- **缺口**：与 Chromium/Edge 的自动像素差分 harness 尚未建立。
  `docs/html5_browser_full_plan.md` 的行动清单第 3 条就是它。已建基线的部分见
  `tests/real_site_tasks/` 的渲染 diff 能力（默认 report-only）。
- 价值：直接对齐主流浏览器行为，避免"自测通过但真实页面不对"。

## 2. 准入门槛（建议 CI 分层执行）

- PR 必须通过 A + B。
- C 目前不在 PR 门禁内（无自动化），只作为发布前的人工环节。
- 当 C 发现差异时：
  1. 先补契约测试（B）描述正确行为；
  2. 再修引擎；
  3. 最后更新必要的视觉基线。

## 3. 回归流程

1. 新增/发现真实页面问题。
2. 抽取最小页面模块（fixture）。
3. 补契约断言：`crates/render-core/tests/`（引擎层）或 `tests/fixtures/real_sites/`（页面层）。
4. 用 `RENDER_DUMP_FRAME` 落帧、`tools/ppm2png.py` 转 PNG，与参考截图人工比对。
5. 修复引擎并提交。

## 4. 为什么这样做

- 仅靠单元测试，容易“局部正确、整页错误”。
- 仅靠截图 diff，定位困难、维护成本高。
- 分层后可同时获得：
  - 快速反馈（A）
  - 页面结构正确性（B）
  - 浏览器一致性（C）

这套方案将测试结果与“是否能正常渲染现代网页”直接绑定，从而让测试更有业务意义。
