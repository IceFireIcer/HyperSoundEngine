# HyperSoundEngine 项目总览

> 版本 0.2.0 · 许可 CC BY-NC-ND 4.0 · 纯 TypeScript 音频 DSP 引擎

## 这是什么

HyperSoundEngine 是一个**纯 TypeScript 实现的实时音频效果引擎**,核心 DSP 内核零 DOM / 零 AudioContext / 零 React 依赖,可在 **Node.js(离线批处理)** 与 **浏览器(实时播放)** 两种环境运行。它不绑定任何特定宿主,可接入任意 Web 应用、Electron 桌面软件、离线转码管线。

设计哲学是 **deep module**:对外只暴露一个极小的 `AudioEngine` 接口(8 个方法),内部封装一条 21 级专业音频处理链,接入方无需理解 DSP 细节。

## 能力一览

### 一、21 级专业音频处理链

引擎内部按固定顺序串联 21 个处理阶段,一次 `process()` 调用即完成全链路处理:

| 序号 | 阶段 | 能力 |
|------|------|------|
| 1 | 响度归一化 | 实时 LUFS 测量驱动增益,目标 -14 LUFS |
| 2 | 3D 环绕 | 轻量立体声旋转(可被调制矩阵驱动) |
| 3 | M/S 立体声宽度 + 人声分离 | 宽度 0–2、voiceBalance 人声/伴奏比例 |
| 4 | Pre-EQ | 5/10/20 段专业均衡(Q 补偿) |
| 5 | Deesser | 齿音抑制(可选外部 sidechain) |
| 6 | Compressor | 动态压缩(可选外部 sidechain) |
| 7 | NightMode | 夜间模式动态压缩 |
| 8 | Delay | 延迟(调制类效果) |
| 9 | Chorus | 合唱 |
| 10 | Flanger | 镶边 |
| 11 | Phaser | 移相(2/4/6/8 级全通) |
| 12 | Tremolo | 颤音 |
| 13 | 混响 | 卷积(非均匀分区 + IR 去周期化)/ 算法(Freeverb)/ FDN 网络 / off |
| 14 | BassEnhancer | 低音增强(谐波合成) |
| 15 | LoudnessComp | 等响度补偿(ISO 226,音量自适应) |
| 16 | IEQ | 智能均衡(频谱特征闭环修正) |
| 17 | [FFT 取样] | 频谱分析与特征提取 |
| 18 | DynamicEq | 自适应动态均衡(频谱包络自动混音,5 带全通交叉) |
| 19 | [LUFS 取样] | 响度统计 |
| 20 | 调制主增益 | LFO/Envelope/MIDI 驱动的 masterGain |
| 21 | Limiter | 前瞻限幅器(true peak,4× 过采样) |

### 二、参数调制矩阵

- **LFO**:正弦/三角/方波/锯齿四种波形,块速率更新
- **Envelope Follower**:起控/释放/强度可调
- **调制路由**:源(LFO/Envelope)→ 目标(masterGain / stereoWidth)+ 深度 + 偏移
- 与 MIDI 自动化共用同一套参数寻址

### 三、MIDI 事件接口 / MIDI Learn

- **实时安全 MIDI 入口**:`sendMidi(events)` 写入预分配环形队列(容量 4096,溢出丢最旧并计数),`process()` 块头消费
- **MIDI Learn 绑定**:CC / Note → 任意参数路径(`AutomationTarget` 白名单,34 个可寻址参数)
- **范围映射**:CC 0–127 线性映射到参数 [min, max],支持反向映射
- **一阶平滑**:防 zipper noise(平滑时间可配)
- **note 驱动**:note on→max / note off→min;布尔参数 on→true / off→false(如效果开关)
- 绑定属配置(`reset` 保留),运行时队列/平滑状态属运行时(`reset` 清空)

### 四、多通道处理

- **AudioBus**:非交错 N 通道缓冲抽象 + 通道级工具(create/fromInterleaved/toInterleaved/copyTo/fill/applyGain/mixFrom/extract/downmixToMono/downmixToStereo)
- **processBus 两种模式**:
  - `downmix`(默认):N 通道下混立体声处理(环绕监听语义)
  - `perChannelPair`:按立体声对 (0,1)(2,3)… 逐对独立处理(独立子引擎池,参数同步),支持 5.1/7.1 各通道独立 DSP

### 五、WAV 文件 I/O

- **encodeWav / decodeWav**:16-bit PCM 与 32-bit Float,多通道,标准 RIFF/WAVE
- 严格校验(坏魔数/缺 chunk/块不对齐/0 声道一律抛错,防注入)
- 解码结果为非交错 Float32Array[],可直接构造 AudioBus 进入处理链

### 六、Sidechain

- `process(inputs, outputs, sidechain?)` 第三参数
- Compressor / Deesser 可选 `sidechainEnabled`,用外部信号驱动包络/检测

### 七、分析与测量

- **EngineStats**:LUFS(积分/瞬时)、LRA、峰值 dB、true peak dB、限幅衰减 dB、引擎延迟样本数
- **EngineAnalysis**:幅度谱(FFT)+ 频谱特征(RMS/ZCR/质心/滚降/平坦度/波峰因子)
- 实时闭环:IEQ 依据频谱特征自动修正

### 八、场景预设与分享串

- **12 个内置场景**:pop/enhance/jazz/dance/classical/livehouse/studio/warm/dts/vocal-stage/night-bass/heavy-bass
- **我的场景**:localStorage 持久化(上限 8 个,快照去 IR)
- **分享串**:base64url(version:checksum:json),FNV-1a 校验 + 白名单字段 + 数值 clamp,非法输入抛错

### 九、浏览器宿主(AudioWorklet)

- `HyperSoundEngineHost`:把引擎接入 Web Audio 图
- 优先 AudioWorklet(渲染线程,低延迟),失败自动回退 ScriptProcessor
- 参数经 `port.postMessage` 下发,stats/analysis 周期回传
- 鸭子类型 AudioNode/AudioContext(Node 测试环境可 stub)

### 十、可选 React UI(调音室)

- 玻璃拟态面板,5 个页签:音效场景 / 均衡器 / 调音器(分享串+导出)/ 分析 / MIDI
- 效果卡片系统 + 参数弹窗(Spatial/Dynamics/Loudness/Modulation/MIDI)
- 经 `HyperSoundEngineUiBridge` 桥接,UI 不直接 import 引擎

## 技术特性

| 特性 | 说明 |
|------|------|
| **实时安全** | `process()` 稳态零分配,无 Math.random / Date / console |
| **确定性** | 同输入同参数 → 同输出(可复现、可测试) |
| **快照语义** | `setParams` 整包替换 + 深拷贝;`getParams` 返回深拷贝 |
| **零依赖** | 核心 DSP 纯 TS;meyda/signalsmith-stretch/soundtouchjs 为可选 |
| **跨环境** | Node 离线 / 浏览器实时 / Electron,同一内核 |
| **深模块** | 对外 8 方法 `AudioEngine` 接口,内部 21 级链封装 |

## 包结构

```
hypersoundengine
├── .                        # 核心:types + dsp/ + engine/ + io/ + analysis/ + offline/
├── /browser                 # 浏览器宿主:HyperSoundEngineHost + AudioWorklet 接线
├── /worklet                 # AudioWorklet 处理器打包入口
├── ui/                      # 可选 React UI(调音室)—— 独立 tsconfig,不随核心打包
├── adapters/waveforge/      # WaveForge 宿主适配(示例,不属于核心包)
├── examples/                # Node 离线 / 浏览器实时 接入示例
└── docs/                    # API / ARCHITECTURE / INTEGRATION_GUIDE / PROJECT_OVERVIEW
```

## 快速开始

```bash
npm install hypersoundengine
npm run build          # 构建核心 + worklet
npm test               # 37 文件 / 368 用例
```

```ts
import { createEngine, createDefaultParams } from 'hypersoundengine'

const engine = createEngine(48000, 2)
const params = createDefaultParams(48000)
params.compressor.enabled = true
params.reverb.enabled = true
engine.setParams(params)
engine.prepare(4096)

const inL = new Float32Array(4096), inR = new Float32Array(4096)
const outL = new Float32Array(4096), outR = new Float32Array(4096)
engine.process([inL, inR], [outL, outR])

console.log(engine.getStats().lufsIntegrated)
```

详细的接线方式与 API 调用见 **[INTEGRATION_GUIDE.md](./INTEGRATION_GUIDE.md)**。
