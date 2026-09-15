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
