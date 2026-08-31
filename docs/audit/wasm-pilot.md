# Phase 5 wasm32 最小试点审计

审计日期：2026-08-30。

> 本文是 2026-08-30 单 Biquad 最小试点的历史审计快照，不代表当前 Phase 5 状态。当前 `hse-wasm` 已提供完整 1–22 级 `HseEngine`、正式 Host 与空间 8 函数 ABI，Chromium AudioWorklet E2E 已通过；真实 SOFA、Firefox 与物理 multichannel 仍待验收。当前状态以 `docs/audit/phase-status.md` 为准。

## 试点边界

本试点新增 `HyperSoundEngineRust/crates/hse-wasm`，它是 `wasm-bindgen` 边界 crate，只直接依赖 `hse-core` 与精确锁定的 `wasm-bindgen = 0.2.127`。库同时产出 `cdylib` 和 `rlib`，首个公开对象为 `HseBiquad`。

`HseBiquad` 在构造阶段固定采样率、滤波器参数和 `maxFrames`，并一次性预分配左右两块 `Vec<f32>`。宿主通过 `left_ptr()`、`right_ptr()` 和 `capacity()` 获得 wasm 线性内存中的 planar 缓冲；`process(frames)` 只对容量内切片作原位处理，超容量返回明确错误。`configure(...)` 更换参数并重建零状态滤波器，`reset()` 清空滤波器状态和两块输入输出缓冲。

浏览器示例位于 `HyperSoundEngineRust/web/wasm-pilot`，处理器注册名固定为 `hypersoundengine-wasm-pilot`。它是独立 AudioWorklet/host 接线，不导入、不替换现有 TS worklet。示例运行时仅使用浏览器标准 API 与 `wasm-bindgen --target web` 生成模块，不含 Node 运行时依赖。

## 消息与故障语义

`configureWasmPilot(node, params, { timeoutMs })` 为每次请求生成唯一 `requestId` 并返回 Promise。同一节点可并发配置；worklet 只以相同 `requestId` 的 `configured` 或 `error` 完成对应请求。默认超时为 2000 ms，调用方也可传入非负有限毫秒数。消息发送异常、`messageerror`、`processorerror`、构造失败、处理失败和超时均会拒绝受影响的待处理 Promise。`resetWasmPilot` 仍为 fire-and-forget。

worklet 对构造、配置、reset 和 process 边界分别捕获异常，并通过 `{ type: "error", phase, code, message, requestId? }` 回传结构化错误。构造失败时 processor 保持可运行的静音失败态；process 失败或 render quantum 超容量时，当前输出先清零、引擎进入静音失败态，异常不会越过 `process()` 回调。由于构造阶段尚未存在配置 `requestId`，构造错误不带请求 ID；宿主收到后会拒绝该节点当时所有待处理配置。

## 构建与检查

CI 精确安装并使用与 crate 一致的 CLI：

```bash
cargo install --locked wasm-bindgen-cli --version 0.2.127
```

release wasm 构建后，CI 在临时目录生成浏览器胶水并执行检查：

```bash
cd HyperSoundEngineRust
cargo build -p hse-core --target wasm32-unknown-unknown --release --locked
cargo build -p hse-wasm --target wasm32-unknown-unknown --release --locked
wasm-bindgen target/wasm32-unknown-unknown/release/hse_wasm.wasm \
  --target web \
  --out-dir "$RUNNER_TEMP/wasm-pilot-pkg"
node ../scripts/check-wasm-pilot.mjs --pkg "$RUNNER_TEMP/wasm-pilot-pkg"
```

`scripts/check-wasm-pilot.mjs` 验证生成的 `hse_wasm.js` 与 `hse_wasm_bg.wasm` 存在、wasm 文件头有效，并以 esbuild 的 browser 平台分别解析/打包生成 glue、host 和 worklet；host/worklet 对本地 `pkg` 的导入在检查时映射到临时生成目录。脚本同时扫描打包元数据中的 Node builtin 导入。

该检查是静态与打包 smoke，不实例化 AudioWorklet，也不声称覆盖浏览器线程、结构化克隆、WebAssembly.Module 跨线程传递或实时音频渲染。`--target web` glue 依赖浏览器加载语义，Node 中能否直接实例化不作为通过条件；真实运行仍需支持 AudioWorklet 与 WebAssembly.Module 结构化克隆的浏览器环境。

## 本机验证结果

- `node --check`：host、worklet、检查脚本均通过。
- `wasm-bindgen 0.2.127`：从已有 release `hse_wasm.wasm` 成功生成临时 web glue。
- 静态/打包 smoke：通过；检查到的 wasm 为 42947 字节，glue、host、worklet 均完成 browser ESM 打包，未发现 Node builtin。
- 本次从当前工作树重新执行 `hse-core` wasm release 构建通过；随后 `hse-wasm` 构建被范围外、未提交的 `engine_chain.rs` 改动阻断：`EngineChainStage` 初始化缺少 `conv` 字段。按任务边界未修改 Rust core/engine-chain，因此不能将本机结果表述为完整 cargo wasm 构建全绿。CI 会在该并行改动修复后执行上述完整门禁。

## 依赖隔离结论

- Rust 核心零反向依赖：`hse-core` 不依赖 `hse-wasm`；依赖方向仅为 `hse-wasm -> hse-core`。
- TS 核心零代码依赖：`src/` 与现有 worklet 未修改，npm 核心构建不导入试点目录。
- 浏览器示例零 Node 运行时：host 与 worklet 均为浏览器 ESM，运行链路为 WebAssembly、Web Audio 和 AudioWorklet 标准 API。
- 试点未引入完整 Rust 引擎链，只验证单个 biquad 从 Rust 核心经 wasm-bindgen 进入独立 AudioWorklet 的最小边界。
