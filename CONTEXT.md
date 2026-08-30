# CONTEXT.md — 领域术语表

> 本文件只放领域语言（术语表），不放实现细节。决策记录见 `docs/adr/`。

## 引擎本体

- **HyperSoundEngine (HSE)**：独立软件 DSP 音频效果引擎。定位是"独立引擎"——自身不是播放器，也不绑定任何宿主。
- **引擎核心 (Core)**：纯 TypeScript 的 DSP 与引擎层，零浏览器/系统依赖，同一 `process()` 服务实时与离线两条路径。
- **宿主层 (Host)**：把引擎核心接进具体运行环境的适配层。已有浏览器宿主；本次新增 Windows 原生宿主。
- **接入方 (Integrator)**：把音频送进引擎处理的外部软件（音乐播放器等）。接入方包含非 JS 技术栈的原生应用。

## 引擎服务进程（Windows 原生宿主）

- **引擎服务进程 (Engine Service)**：承载引擎核心的独立操作系统进程，独占音频设备管理权。接入方通过它使用引擎，而非在进程内嵌入引擎。
- **控制面 (Control Plane)**：接入方向引擎服务进程下发参数、查询状态、管理会话的通道。
- **音频入口 (Audio Ingress)**：播放器音频进入引擎服务进程的通道，两种形态并存：
  - **回环拦截 (Loopback Intercept)**：引擎捕获指定渲染端点的 WASAPI loopback 混音；也可用 capture 模式直接打开虚拟音频缆的捕获端点。捕获源与最终渲染端点独立选择。
  - **推流 (Push Stream)**：接入方自行解码，按引擎服务进程的音频流协议推送 PCM 块；每条流是一个独立**会话 (Session)**。
- **渲染输出 (Render Output)**：引擎处理后音频的最终出口（WASAPI 设备）。回环拦截与推流会话均经 WASAPI 渲染；项目不提供 ASIO 后端。
- **混后处理 (Mix-then-Process)**：多会话并存时的语义——各流先求和，再进处理链（与回环拦截的系统混音语义一致）；不做逐流独立 DSP。

## 双支线（原生化决策产物）

- **TS 支线**：`HyperSoundEngine/` 的纯 TypeScript 引擎实现，承担浏览器宿主、Node 离线路径与 golden 参考。
- **Rust 支线**：全量重写的原生实现，承担 Windows 引擎服务进程与性能目标；与 TS 支线功能对等。
- **规格 (Spec)**：两支线共同的行为基准——给定/当/则描述 + 测试向量，存放于工作区 `specs/`。功能完成的定义 = 规格落定且两支线双双通过。
- **测试向量 (Test Vector)**：规格附带的数据夹具（输入 PCM + 参数 → 期望输出 PCM），由规格所有，任何支线不得单方面修改。
- **对拍 (Parity Run)**：用同一测试向量在两支线分别执行并比对，相对容差 1e-6。跨实现不要求逐位一致。
- **冻结基线 (Frozen Baseline)**：已入库向量的期望值永不修改；行为变更 = 新增向量（MINOR）或整体替换（MAJOR）。基线自 TS 支线现行为 Bootstrap 导出，唯一生成入口为仓库根 `scripts/export-vectors.mjs`。
- **双绿门禁 (Dual-Green Gate)**：功能"完成"的判定口径——同一组向量在 TS 门禁测试（vitest `test/spec-vectors.test.ts`）与 Rust 对拍 harness（`hse-parity`）上双双通过。
- **兼容契约 (Compatibility Contract)**：接入方可在两支线间无损切换的三层保证——`AudioEngine` 接口语义、参数模型/场景预设/分享串格式、引擎服务进程控制协议。

## 性能与实时

- **实时安全 (Realtime Safety)**：音频回调路径零分配、零锁、零系统调用的工程纪律。
- **双路径一致 (Dual-Path Consistency)**：实时播放与离线导出必须经过同一核心处理逻辑，产出一致结果。
- **确定性 (Determinism)**：同输入同参数必同输出；核心内禁用随机、时钟、控制台输出。
