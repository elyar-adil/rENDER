# test262 接入

rENDER 的 JavaScript 一致性门禁使用官方 [tc39/test262](https://github.com/tc39/test262) 测试，并固定到：

```text
5ef1e5723be95296f36afb0386676fed0205869c
```

该 revision 对应 2026-07-29 的 `main`。固定 revision 是必要条件：上游测试持续变化，未固定版本的 pass/fail 数字不可比较。

## 运行

在仓库根目录执行：

```bash
cargo test --release -p render-core --test test262 pinned_test262_manifest_reports_a_real_baseline -- --ignored --exact --nocapture
```

全量测试默认标记为 `#[ignore]`，因此 `cargo test --workspace` 只运行 runner 自身的 10 个自测，不会在 debug profile 下扫描全部 103,199 个变体。这个区分是必要的：生成的 RegExp property-escape 用例在 debug 下会超过每文件 30 秒的硬超时，在 release 下则远低于它（本机 `ASCII.js` 实测 debug 40 秒、release 7.5 秒）。`tools/check.sh` 和 CI 以 release 运行全量门禁，记录的数字也来自 release。

runner 默认使用逻辑 CPU 一半、最多 8 个隔离 worker。协调器逐文件动态派发任务；每个 worker 拥有独立进程、DOM 和 Runtime。单文件超过硬超时，或 worker 出现 panic/崩溃时，协调器会终止该进程、记录 `timeout`/`crash`，再启动新 worker 继续队列，避免一次异常中断整轮扫描。

可使用以下环境变量控制执行：

- `RENDER_TEST262_WORKERS`：worker 数量；
- `RENDER_TEST262_TIMEOUT_SECS`：单个官方测试文件的硬超时，默认 30 秒；
- `RENDER_TEST262_RUN_DIR`：结果目录；再次使用相同目录会读取 `completed.txt` 和 `results.tsv`，从未完成文件继续；
- `RENDER_TEST262_MAX_FILES`：仅用于 runner 冒烟测试和吞吐基准，不可用于报告全量通过率。
- `RENDER_TEST262_PATH_PREFIX`：仅运行指定测试目录，例如 `language`、`built-ins/Array`；与 `MAX_FILES` 组合时先过滤再截断。

runner 递归发现固定 revision 的 `third_party/test262/test/**/*.js`，当前共 53,869 个官方 JavaScript 文件。每个测试使用新的 `Dom` 和 `JsRuntime`，并将结果分为：

- `pass`：测试按 test262 元数据完成；
- `fail`：出现意外异常、解析失败或 negative 期望不匹配；
- `skip`：清单或测试结构本身不应作为独立用例执行，例如 `_FIXTURE`；
- `unsupported`：测试需要当前引擎尚未实现的模块、异步 `print` 完成协议或其他 host 能力；
- `timeout`：单个官方测试文件超过硬超时；
- `crash`：隔离 worker 在完成当前文件前异常退出。

最终目标是固定 revision 下所有适用变体 100% 通过；`skip` 必须由官方测试结构决定，`unsupported` 必须有明确的尚未实现规范能力，且两者都不是终态。摘要同时按 `language`、`built-ins`、`intl402`、`annexB`、`staging` 等顶级目录分层，避免总通过率掩盖关键语言能力。

runner 已实现的执行骨架遵循官方 `INTERPRETING.md`：

1. 解析 `/*--- ... ---*/` frontmatter；
2. `raw` 用例不注入 harness、不改写源码、只运行一次；
3. 普通用例按 `flags` 生成 sloppy/strict variant；
4. 默认 `assert.js`、`sta.js` 预编译一次，并在每个测试的独立 Realm 中按顺序执行；
5. 测试脚本与 harness 分开编译和执行，strict directive 只作用于测试脚本；
6. parse-negative 先编译测试脚本，避免 harness 初始化错误污染预期解析结果；
7. 核对 `negative.phase` 与错误类型；
8. `module`、`async` 和 `_FIXTURE` 先显式分类，不伪造通过率。

2026-10-22 在 release profile 下完成的全量基线（默认 worker 数，865.67 秒）：

```text
files=53869
variants=103199
pass=90765
fail=12127
skip=294
unsupported=7
timeout=6
crash=0
```

分类计数：

```text
annexB:    pass=1209  fail=168   skip=0   unsupported=0 timeout=0 crash=0
built-ins: pass=42938 fail=4361  skip=3   unsupported=7 timeout=0 crash=0
intl402:   pass=162   fail=6552  skip=0   unsupported=0 timeout=0 crash=0
language:  pass=43799 fail=694   skip=283 unsupported=0 timeout=0 crash=0
other:     pass=230   fail=2     skip=0   unsupported=0 timeout=0 crash=0
staging:   pass=2427  fail=350   skip=8   unsupported=0 timeout=6 crash=0
```

检查入的 `crates/render-core/tests/test262-baseline.tsv` 已按这次 release 运行重建（264 个桶，90,765 pass），门禁输出 `264 buckets ok`。此前的检查入基线为 47,116 pass（2026-10-09，提交 `cba591c`）。从那时到本次运行之间有 228 个提交修复了解析器、内建对象和 async 用例；release 运行又避免了 debug 下 RegExp property-escape 的假超时。这次提升是两者共同作用，不能全部归因于运行 profile。旧基线文件合计 98,096 个变体，当前 runner 记录 103,199 个；固定 revision 未变，差异来自 runner 对 module/async/fixture 用例的分类与执行口径变化（例如 `async` 用例不再一律记为 unsupported）。

当前最大的失败簇是 `intl402`（6,552 fail）和 `built-ins` 的 `unexpected Throw`/`Type`/`Reference`；`language` 只剩 694 fail。6 个 timeout 全部在 `staging`，crash 为 0。

这只是固定 revision、当前 runner 和当前引擎边界下的工程基线，不是完整 ECMAScript 合规率。后续以失败簇为单位修复，禁止按具体测试路径写特判。基线文件是回归地板：任何桶的 pass 不得低于该次记录。

全量扫描不应因普通 `fail` 或 `unsupported` 中断，而是输出真实总量、分类计数、失败簇和有限样本，作为适配优先级依据；但引擎 panic、资源失控或 runner 基础设施错误必须使扫描失败。严格回归门禁仍需从全量结果中维护已支持集合，确保已经适配的语义不会退化。不得把支持集合的通过率描述为完整 test262 通过率。

## 更新固定版本

不要直接跟随 `main` 修改基线。更新版本时：

1. 修改 `tools/fetch-test262.sh` 中的 `TEST262_REVISION`；
2. 运行脚本重新下载官方归档；
3. 审查 manifest 路径是否仍存在；
4. 运行 test262 集成测试并记录新摘要。
