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

**进一步证伪(同日)**:新增 `crates/render-core/examples/dom_dump.rs`(页面跑完内联脚本后把 DOM 序列化回 HTML)。用内联脚本执行后的 DOM 离线渲染 + 同 3 个 CSS:search-m 子树依旧完美(form/input/红按钮全在),与原始 DOM 渲染逐像素一致(div 数不变)。**假设 A(内联 JS 改 DOM)证伪**。剩余唯一差异:在线还成功执行了 5 个外链脚本(jquery-1.6.4、wl.js、o2_ua.js+event.js、两个 inline),它们的 DOM 改动(class 注入等)是最后未复现的变量。**下一步明确**:仿 render-js examples/bilibili_diag.rs 写 jd_diag.rs——manifest.txt 映射外链脚本 URL→本地文件,完整回放后序列化 DOM,再离线渲染对比。若回放后仍正常,则问题必在浏览器侧 commit/绘制链路(回到假设 B,重点查 expected_render 竞态:8 次渲染只有 3 帧 commit,大量结果被 drain_latest 丢弃,可能存在"带样式渲染结果被丢弃、裸文本旧帧当最终帧"的直接证据链)。

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
