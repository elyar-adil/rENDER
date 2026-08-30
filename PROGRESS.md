# 进度记录 — 百度页面支持（2026-08-24）

## 已完成

### 栈溢出修复（render-browser/src/main.rs）
- `main()` 在 512MB 栈的专用线程上启动 `browser_main`（解释器/解析器递归深，默认主线程 1MB 栈不够）。
- winit 0.30.13 要求：非主线程创建事件循环必须调用 `EventLoopBuilderExtWindows::with_any_thread(true)`（注意不是 `any_thread()`，那是 Window handle 的方法）。
- 错误处理改为 `Result<(), String>`（`Box<dyn Error>` 不满足线程边界 Send + Sync）。
- 实测：`cargo run -p render-browser -- https://baidu.com` 正常启动、加载、执行脚本。

### JS 引擎（本会话早前完成）
jQuery 与 ESL AMD 加载器完整执行；baidu_diag 报错从 23 → 10。
正则引擎（js/regex.rs）、ASI、标签语句、var 提升修复（VariableList）、
`new A.B.C()` 成员链、原始值包装（Number/Boolean/Symbol/Date）、
隐式全局、typeof 未声明、数组泛型接收者、大量 DOM API。

## 剩余 10 个脚本错误（诊断日志）

| 脚本 | 错误 | 状态 |
|---|---|---|
| sbase / hotsearch ×2 | `.1 of null` | 待查（agent A 网络错误未完成）|
| es6-polyfill | `.primitive method receiver of undefined` | 待查（agent B 未完成）|
| polyfill_9354efa | array length is not a supported integer | ✅ 已定位（见下）|
| instant_search | `.match of undefined` | 待查 |
| inline | `.indexOf of undefined` | 待查 |
| all_async_search / min_super | value is not a constructor | 待查（agent D 未完成）|
| hectorstatic | `.apply of undefined` | 待查 |

## Agent C 调查报告（array length）— 已修复（2026-08-30）

**产生点**：`runtime.rs:4846 array_length()` 严格要求 Number 整数；所有 11 个数组
原生方法都经它取 length。写入侧 `set_member` 没有 Array+length 分支（存原始值不校验）。

**触发点**（polyfill_9354efa.js = es5-shim + tslib）：第 13 行 splice 特性检测
`Array.prototype.splice.call({}, 0, 0, 1)` —— 普通对象无 length。规范要求
ToLength(undefined)→0，我们直接抛 TypeError。

**修法**：
1. `array_length` 改用 ToLength 语义（to_number→NaN/∞/<0 钳 0，向零截断）。
2. `set_member` 增加 Array+"length" 分支：非法值拒绝，缩容截断元素。
3. `construct_dispatch` 与普通调用分派补齐 `ObjectHost::ArrayConstructor`。
4. `Array()` / `new Array()` 支持空参数、单数字长度、单值和多值构造。
5. `unshift` 返回新数组长度；新增运行时回归测试覆盖上述行为。

## 其他已知问题

- 性能修复（2026-08-30）：浏览器事件线程不再同步执行页面布局/栅格化；鼠标移动不再无条件重绘整窗；等待中的 `setInterval` 不再触发 16ms 空转。

- data: URI 图片不支持（网络层 UnsupportedScheme），NodeId(476) 反复报错。
- @font-face/@keyframes/@media 已解析未评估。
- application/json script type 日志噪音（行为符合规范，可降级日志级别）。
- `jquery-init-shape` 探针偶发 "maximum call depth exceeded"——深度预算偏紧。
- 渲染层：搜索框缺失/布局乱（CSS 布局引擎能力不足 + 脚本级联失败）。

## 工具与环境备忘

- 离线语料：`%TEMP%\opencode\baidu_http.html`、`baidu_assets\manifest.txt`、`inline_scripts\`。
- 探针：`cargo run -p render-core --example js_probe -- <file.js>`（512MB 栈线程）。
- 分割工具：`examples/jquery_bisect.rs`；诊断：`examples/baidu_diag.rs`（RENDER_DUMP_INLINE=1 导出失败的内联脚本）。
- 调试开关（临时）：RENDER_JS_TRACE / RENDER_JS_DEPTH / RENDER_JS_BINDINGS。
- Agent C 复现脚本：`%TEMP%\opencode\probe\c\repro_polyfill.js` 等。
