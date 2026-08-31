# HyperSoundEngine 项目总览

> 当前源码版本：1.5.1 · 许可：CC BY-NC-ND 4.0 · TypeScript / Rust 双支线实时音频 DSP 引擎

## 1. 项目定位

HyperSoundEngine 是不绑定特定播放器或 UI 的音频处理引擎。两条实现共享 `specs/` 行为契约，但源码和构建保持独立：

- **TypeScript 支线**：npm 包、Node 离线处理、浏览器 Host、TS AudioWorklet 与空间参考实现。
- **Rust 支线**：原生 DSP、HRTF renderer、Windows `hse-service`、WASAPI 后端和浏览器 wasm 引擎。

接入新项目先阅读 [`INTEGRATION.md`](INTEGRATION.md)。API 类型参考见 [`API.md`](API.md)，当前阶段与验收边界见 [`audit/phase-status.md`](audit/phase-status.md)。

## 2. 当前能力

### 2.1 22 级处理链

第 1–21 级为共享主链：响度归一化、3D 环绕、M/S、EQ、Deesser、Compressor、NightMode、Delay、Chorus、Flanger、Phaser、Tremolo、四路混响、BassEnhancer、LoudnessComp、IEQ、FFT 分析、DynamicEq、LUFS、调制主增益和 Limiter。

第 22 级为空间音频，默认 `spatial.mode='off'`。Rust 注入 HRTF grid 后支持：

- `instant`
- `headLocked`
- `world`
- `stage`

空间能力包括完整 listener 姿态、轨迹、稳定对象 slot、距离与空气吸收、Doppler、遮挡、声源大小、nearest/spherical 插值、time/partitioned 双耳卷积、早期反射、FDN 房间和 ambience。最终输出仍为双耳立体声。

### 2.2 多通道

- TS core `processMulti()`：3–8 路非交错输入转双声道。
- Browser Host：支持 2/6/8 路 discrete 输入，输出固定 2 路。
- `HseAudioBus.processBus()`：非实时 downmix 或 per-channel-pair 便利路径。
- 当前不提供 5.1/7.1 物理多声道输出。

### 2.3 文件、场景和分享串

- WAV：16-bit PCM / 32-bit float，legacy/standard 双格式。
- 场景：12 个内置全参数快照。
- 分享串：当前编码格式为 `HSE2` + Crockford Base32 差异载荷；解码继续兼容历史 v1 全量载荷。

### 2.4 浏览器宿主

`HyperSoundEngineHost` 支持：

- TS AudioWorklet，失败时可回退 ScriptProcessor。
- 完整 Rust/WASM 1–22 级引擎。
- SOFA bytes 或预解析 HRTF grid。
- 参数更新时预建新节点，等待 `ready`，以零增益接入并预滚一个 128-frame render quantum，再交叉淡变替换。

运行期不向 AudioWorklet 发送参数重建消息；完整参数快照只在节点构造阶段通过 `processorOptions` 应用。

### 2.5 Windows 原生服务

`hse-service` 提供：

- localhost WebSocket JSON-RPC 控制面。
- WASAPI shared/exclusive capture/render；loopback 仅 shared。
- 立体声 f32le PCM 推流会话。
- 多源混后处理，再进入完整 Rust 1–22 级链。
- HRTF 控制路径加载、参数块边界热换。
- 双环深度、高水位、帧延迟估算和 xrun 统计。

服务处理后的 PCM 输出到 WASAPI 设备，不通过 WebSocket回传。完整协议见 [`../specs/service/control-plane.md`](../specs/service/control-plane.md) 和 [`../specs/service/push-stream.md`](../specs/service/push-stream.md)。

## 3. 共享规格和自动门禁

当前基线：

- 25 份共享规格：17 DSP + 4 engine + 1 I/O + 3 spatial。
- 音频冻结向量：72 组 / 144 文件，Rust 72/72 PASS。
- 空间结构夹具：world-listener 14/14 + renderer/ABI 14/14 = 28/28 PASS。
- Phase 4 固定参数扫描：40/40 PASS。
- Release 实时分配门禁：默认链、全开链、长 IR 和 stage 22。
- Chromium 正式 wasm AudioWorklet E2E。
- Windows 无设备 core/WASAPI/service/parity 门禁。

最新验证结果应以 `main` 对应的 GitHub Actions CI 和 [`audit/phase-status.md`](audit/phase-status.md) 为准，不在本页复制易漂移测试总数。

## 4. 接入方式

| 场景 | 接口 |
|---|---|
| Node/Electron 离线 PCM | `hypersoundengine` / `AudioEngine` |
| Web Audio 实时处理 | `hypersoundengine/browser` / `HyperSoundEngineHost` |
| 浏览器 Rust DSP | Host `engineBackend:'wasm'` |
| 任意语言 Windows 原生接入 | `hse-service` JSON-RPC + PCM WebSocket |
| 原生程序只用空间 renderer | `hse-wasm` 空间 C ABI |

`hse-napi` 尚未实现。Node/Electron 若需要 Rust 完整引擎，应通过 `hse-service`，或在浏览器渲染进程使用 wasm Host。

## 5. 快速开始

从源码构建：

```bash
git clone https://github.com/IceFireIcer/HyperSoundEngine.git
cd HyperSoundEngine
npm install
npm run build
npm test
```

Node 离线示例：

```bash
node examples/node-offline.mjs
```

Rust 服务：

```bash
cd HyperSoundEngineRust
cargo run -p hse-service
```

默认控制地址为 `ws://127.0.0.1:4780/`。其他项目的完整接入顺序、错误处理和 PCM 帧布局见 [`INTEGRATION.md`](INTEGRATION.md)。

## 6. 当前阶段

- Phase 0–1：完成。
- Phase 2：主体完成，正式播放器/VB-CABLE 真机出口待验收。
- Phase 3：实现完成，双推流加非零真实 capture/loopback 联合出口待验收。
- Phase 4：自动实现完成，真实 shared/exclusive 延迟、xrun 和 CPU 待验收。
- Phase 5：主体实现完成；真实 SOFA 自动门禁与 Firefox E2E 未完成，物理 multichannel 输出尚未实现。

## 7. 发布状态

1.5.1 的源码和自动门禁可以作为**源码型 GitHub Release 候选**。npm、正式 Windows 二进制和 crates.io 的发布条件尚未满足，具体证据和阻断项见 [`RELEASE_READINESS.md`](RELEASE_READINESS.md)。

## 8. 许可

项目主体代码采用 [CC BY-NC-ND 4.0](../LICENSE)。第三方组件分别遵循各自许可证，见 [`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)。
