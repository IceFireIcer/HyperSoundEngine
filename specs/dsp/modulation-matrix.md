# 规格：modulation-matrix —— 参数调制矩阵（LFO / 包络跟随 → 控制率目标值）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/modulation.ts`
> （`ModulationMatrix` / `Lfo` / `EnvelopeFollower`）；路由与调制源类型字段名以
> `src/types.ts`（`ModulationRoute` / `LfoShape`）为准，本规格不得臆造字段。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。
> 本模块是**向量驱动模型的首个控制率 Stage 形态**（§4.4）：f32 四段布局、JSON 字段、
> 容差公式**不变**（总纲 §3.3/§3.5 继续全文适用），分块语义按本规格 §4.4 重解释。

---

## 一、模块概述

- **定位**：控制率调制矩阵——LFO（sine/triangle/square/saw 四形态）与包络跟随器
  （双声道联合峰值检测 + 一阶 attack/release 平滑）经路由表求和到两个内置目标
  `masterGain` / `stereoWidth`，**每块各产出一次**目标值（控制率，非逐样本）。
  引擎接线（`HyperSoundEngine.ts`）：矩阵在 `process()` 块头推进（先于各级 stage），
  产物 `masterGain` 由链尾 **`mod-master-gain` 阶段**逐样本乘到 L/R、`stereoWidth` 由
  **`mid-side` 阶段**作为宽度参数消费（仅 `modulation.enabled` 时）。
- **出处**：自研基础件。LFO 波形与包络跟随（峰值检测 + 一阶平滑）为音频处理公有知识，
  无第三方代码。
- **确定性**：无随机、无 Date、无 console；同输入同参数同块长同输出。
- **采样率**：构造时固定；`fs ≤ 0` 抛 `Error('invalid sample rate')`。进入 LFO 相位步进
  （`rateHz·n/fs`）与包络 attack/release 系数。
- **无 `enabled` 字段（行为事实）**：`ModulationSettings.enabled` 属引擎层门控
  （决定引擎是否调用矩阵、`mod-master-gain` 阶段是否激活）；`lfo.enabled` /
  `envelope.enabled` 在引擎接线中同样**不被任何代码消费**（引擎只向矩阵转发
  `shape/rateHz/depth` 与 `attackMs/releaseMs/amount`，源码 `setRoutes`/`setLfoParams`/
  `setEnvelopeParams` 三调用）。模块自身恒处理。向量 `params` 不得包含任何 `enabled` 字段。

## 二、接口签名（事实标准摘录）

```ts
export type LfoShape = 'sine' | 'triangle' | 'square' | 'saw'   // src/types.ts

export class Lfo {
  constructor(sampleRate: number, shape?: LfoShape, rateHz?: number, depth?: number)
  setParams(shape: LfoShape, rateHz: number, depth: number): void
  processBlock(n: number): number        // 推进 n 个样本并返回当前归一化输出（-1..1）
  reset(): void                          // phase = 0
}

export class EnvelopeFollower {
  constructor(sampleRate: number, attackMs?: number, releaseMs?: number, amount?: number)
  setParams(attackMs: number, releaseMs: number, amount: number): void
  processBlock(l: Float32Array, r: Float32Array, n: number): number  // 返回块尾包络（已乘 amount）
  reset(): void                          // env = 0
}

export class ModulationMatrix {
  constructor(sampleRate: number, routes?: ModulationRoute[],
              lfo?: { shape: LfoShape; rateHz: number; depth: number },
              envelope?: { attackMs: number; releaseMs: number; amount: number })
  setRoutes(routes: ModulationRoute[]): void
  setLfoParams(shape: LfoShape, rateHz: number, depth: number): void
  setEnvelopeParams(attackMs: number, releaseMs: number, amount: number): void
  processBlock(l: Float32Array, r: Float32Array, n: number): { masterGain: number; stereoWidth: number }
  reset(): void                          // lfo.reset() + env.reset()
}
```

进入向量驱动的只有 `ModulationMatrix`（经 §4.4 的 Stage 包装）；`Lfo` /
`EnvelopeFollower` 的独立行为经矩阵整体冻结，不单设向量。

## 三、参数表（向量 `params` 快照字段）

向量 JSON 的 `params` 对象固定使用以下三字段（**无 `enabled`**，见 §一）；采样率 fs 不进
params，取自向量顶层 `sampleRate` 字段：

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `routes` | `ModulationRoute[]` | 见下 | `[]` | 每项 `{source, target, amount, offset?}`；`source ∈ {'lfo','envelope'}`、`target ∈ {'masterGain','stereoWidth'}`；`amount` 不钳制²；`offset` 省略时按 0（`?? 0`） |
| `lfo` | `{shape, rateHz, depth}` | 见 §4.1 | sine / 1 / 0.5（构造缺省） | `rateHz = max(0, rateHz)`（负值→0，相位冻结）；`depth` 钳制 `[0,1]`；shape 枚举外按 `sine`（switch default） |
| `envelope` | `{attackMs, releaseMs, amount}` | 见 §4.2 | 10 / 200 / 0.5（构造缺省） | `attackMs/releaseMs = max(·, 0.05)` ms；`amount` 钳制 `[0,1]` |

¹ 「默认值」为 `ModulationMatrix` 构造函数对缺省子对象的取法；向量必须提供完整三字段。
² 路由 `amount` 不钳制（超界值经目标钳制兜底，case3 固化）。

## 四、处理语义（两支线必须逐字一致）

### 4.1 LFO（控制率相位推进）

- **相位每块推进一次**（非逐样本）：`phase = (phase + rateHz·(n/fs)) % 1`，`n` 为本块帧数，
  f64 取模；
- **值在推进之后采样**（实证 §4.5.1）：首块返回值即 `value(推进后相位)·depth`，不是相位 0
  的值；
- 波形（p = phase）：
  `sine = sin(2πp)`；`triangle = 4·|p − 0.5| − 1`；`square = p < 0.5 ? 1 : −1`；
  `saw = 2p − 1`；
- 输出 = `value()·depth`（双极性 −1..1，depth 钳制后）。

### 4.2 包络跟随器（逐样本，双声道联合峰值）

```text
逐样本（i = 0..n−1）：
  e = max(|l[i]|, |r[i]|)                    # 双声道联合峰值，非各自独立包络
  e > env ? env += attackCoef·(e − env) : env += releaseCoef·(e − env)
attackCoef = 1 − exp(−1 / ((attackMs/1000)·fs))     # attackMs 先按 max(·,0.05) 钳制
releaseCoef 同构（releaseMs）
返回 env · amount                            # 状态 env 不含 amount（返回时才乘）
```

- 静音输入（e = 0、env = 0）走 release 路径但差值为 0：**包络精确保持 0**（实证 §4.5.5）。

### 4.3 路由求和与钳制

```text
lfoVal = Lfo.processBlock(n)                 # 两源每块无条件推进（无路由也推进）
envVal = EnvelopeFollower.processBlock(l, r, n)
masterGain  = 1；stereoWidth = 1
对每条 route：v = src·amount + (offset ?? 0)      # src: 'lfo'→lfoVal，'envelope'→envVal
  target = 'masterGain' → masterGain += v；否则 stereoWidth += v
masterGain  = clamp(masterGain, 0, 4)
stereoWidth = clamp(stereoWidth, 0, 2)
返回 { masterGain, stereoWidth }
```

- `stereoWidth` 的钳制域是 `[0, 2]`（与 masterGain 的 `[0, 4]` 不同）；
- 两源状态推进与路由表内容无关（无任何路由时 LFO 相位与包络状态照常更新）。

### 4.4 向量驱动模型（控制率 Stage 形态——本规格特有的扩展，两支线必须逐字一致）

总纲 §3.4 的流式分块语义对本模块重解释如下（f32 四段布局、JSON 字段、容差公式不变）：

1. **实例化（引擎接线顺序）**：`new ModulationMatrix(sampleRate)` →
   `setRoutes(params.routes)` → `setLfoParams(params.lfo.shape, params.lfo.rateHz, params.lfo.depth)`
   → `setEnvelopeParams(params.envelope.attackMs, params.envelope.releaseMs, params.envelope.amount)`；
   实例为全新零状态，不额外调用 `reset`（总纲 §3.4）。
2. **每块（Stage = 推进 + 应用）**：复制输入块 → `processBlock(bl, br, len)` 取
   `masterGain` → 逐样本 `outL[i] *= masterGain; outR[i] *= masterGain`。即：**本模块的
   每块音频可观测语义 = 推进矩阵（LFO 相位、包络状态）并把调制后的 masterGain 逐样本乘
   到 L/R**——对应引擎 `mod-master-gain` 阶段的逐样本乘法。
3. **包络跟踪增益前信号**：引擎中矩阵在块头推进（先于各级）、增益在链尾应用——包络永远
   跟踪**增益前**的输入；独立模块形态下无其他级，驱动器在乘法之前调用 `processBlock`，
   包络读取的即本块增益前输入（两者一致）。
4. **stereoWidth 产物不入向量**：宽度路径是引擎组合（矩阵 → `mid-side` 阶段
   `setParams(width)`），属引擎接线行为，不是本模块的音频可观测输出；本规格仅以 case3
   固化「存在 stereoWidth 路由时 masterGain 路径不受影响」的惰性事实。宽度调制行为若需
   向量，属引擎级组合向量，不在本模块范围。
5. **输出依赖 blockSize（控制率——模块特有行为事实）**：增益按块常量、LFO 相位按块推进，
   不同 blockSize 产生不同增益轨迹（实证 §4.5.4）→ **向量固定 blockSize，两支线必须按
   同一块长回放**（loudness-comp §GWT-LC-07 同款条款）。
6. **一句话定义**：**modulation-matrix Stage：每块先推进矩阵、再把块率 masterGain 逐样本
   乘到 L/R；包络跟踪增益前输入；宽度路径不入向量。**

### 4.5 实证行为记录（导出工具冻结前逐项验证）

1. **LFO 首块值 = 推进后相位采样**：fs=48000、rate 4Hz、块 256 → 首块值
   `sin(2π·4·256/48000) ≈ 0.13364`（非 `sin(0)=0`）；第二块
   `sin(2π·(4·512/48000)) ≈ 0.26488`（相位逐块累进、f64 取模）。
2. **rateHz/depth 钳制**：`rateHz=-5 → 0`（相位冻结于 0，sine 值恒 0）、
   `depth=3 → 1`（输出值被 1 封顶）。
3. **无 masterGain 路由 → 逐位恒等**：仅含 stereoWidth 路由时 `masterGain` 恒精确 1，
   `x·1` 逐位还原输入（含 ±0 语义）——本模块的逐位锚点形态。
4. **钳制 0/4 双向可达（case3）**：`1 + 3.0·saw + 1.5·env − 0.5` 在 6000 帧/384 块长下
   实测 5 块触 0、1 块触 4。
5. **包络静音保持 0**：全零块下 env 精确保持 0（release 路径零差值）。
6. **processBlock 不改写输入缓冲**（Lfo 不触碰 l/r，EnvelopeFollower 只读）。
7. **包络为联合峰值**：`e = max(|L|,|R|)`，立体声联动（与 compressor 的联合包络同构）。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/modulation-matrix.<case>.json` 冻结夹具 + 容差公式
> 判定（README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-MM-01：LFO→masterGain 块率幅度调制
- **给定**：仅一条 lfo→masterGain 路由（amount < 1）、sine 波形、depth=1
- **当**：按 §4.4 分块驱动（增益按块常量乘到整块）
- **则**：增益轨迹为 LFO 的块率阶梯采样（首块即推进后相位值），输出 = 输入 × 逐块增益；
  精确波形以冻结向量界定。驱动器若逐样本推进 LFO 相位或以推进前相位采样必然超差

### GWT-MM-02：包络跟随 attack/release 增益轨迹
- **给定**：仅一条 envelope→masterGain 路由（amount=1、offset 使 masterGain = 包络值）；
  输入为「头部静音 → 有声突发 → 尾部静音」三段式
- **当**：按 §4.4 分块驱动
- **则**：静音段包络精确保持 0（增益 0、输出整块为零）；突发段包络沿 attack 系数从 0
  爬升、尾段沿 release 系数衰减；联合峰值语义（max(|L|,|R|)）由左右去相关激励固化；
  逐块增益轨迹以冻结向量界定

### GWT-MM-03：双源叠加与双向钳制
- **给定**：lfo 与 envelope 两条 masterGain 路由（大 amount + 偏移），合成值越出 [0,4]
- **当**：按 §4.4 分块驱动
- **则**：masterGain 触及 0 与 4 两个钳制边界（下限静音块、上限增益封顶块均存在）；
  未钳制块为双源线性叠加；精确轨迹以冻结向量界定

### GWT-MM-04：stereoWidth 路由对 masterGain 路径惰性
- **给定**：路由表同时含 stereoWidth 目标路由（本规格向量作用域外的目标）
- **当**：按 §4.4 分块驱动
- **则**：masterGain 路径与不含该路由时完全一致（宽度产物不进入音频输出）——驱动器若把
  stereoWidth 混入音频路径（如误当增益应用）必然超差

### GWT-MM-05：无 masterGain 路由恒等锚点（逐位）
- **给定**：路由表仅含 stereoWidth 目标路由（含非零 amount/offset）
- **当**：按 §4.4 分块驱动
- **则**：masterGain 恒精确 1，输出与输入**逐位一致**（左右声道皆然，含 ±0）——最强跨
  实现精度锚点，捕获任何把宽度路由误入增益、或增益求和基线偏离 1 的实现

### GWT-MM-06：blockSize 是行为参数（控制率；模块特有）
- **给定**：任意含 masterGain 路由的参数
- **当**：比较不同 blockSize 的回放
- **则**：输出**依赖 blockSize**（增益按块采样、LFO 相位按块推进；实证差异显著）——
  两支线对拍必须按向量固定 blockSize 回放

### GWT-MM-07：LFO 四形态波形与钳制（由冻结向量整体界定 + 单元测试覆盖）
- sine/saw 形态随 case1/case3 入向量；triangle/square 的波形公式由 §4.1 固化、
  其数值行为经「同参数同块长重放一致」与单元测试覆盖，不单设向量

### GWT-MM-08：极值参数钳制无数值事故（由单元测试覆盖，不入向量）
- **给定**：rateHz 负值、depth/amount 越界、attackMs/releaseMs 低于下限、routes 为空表
- **当**：构造与常规激励
- **则**：按 §三钳制生效；全程无 NaN / Infinity、有界；空路由时 masterGain 恒 1

### GWT-MM-09：reset 复现性与抛错路径（由单元测试覆盖，不入向量）
- **给定**：已推进若干块的实例；非法采样率
- **当**：reset() 后以同参数同输入重放；以 fs ≤ 0 构造
- **则**：重放逐位一致（LFO phase=0、env=0 起步）；构造抛 `Error('invalid sample rate')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/modulation-matrix.<case>.json` + 同名 `.f32`
冻结夹具为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具
产出并冻结的内容为唯一判据。格式契约见 [`specs/README.md`](../README.md) §三（本模块按
§4.4 Stage 模型驱动）；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **LFO 块率调制**：单条 lfo→masterGain 路由（sine、中低 rateHz、depth=1、amount<1），
   增益阶梯轨迹整段冻结；左右为去相关的正弦叠加/伪噪声（包络源有激励但不入路由）
   （GWT-MM-01）；
2. **包络 attack/release**：单条 envelope→masterGain 路由（masterGain = 包络值），输入为
   「静音 → 突发 → 静音」三段式，包络从 0 爬升再衰减的逐块增益轨迹整段冻结；多采样率
   44100 覆盖 attack/release 系数随 fs 变化（GWT-MM-02）；
3. **双源叠加 + 双向钳制 + 宽度路由惰性**：lfo（saw）与 envelope 双路由大深度叠加，
   masterGain 触 0 与 4 双钳制边界；同时含 stereoWidth 目标路由（对输出无贡献）
   （GWT-MM-03/04）；
4. **恒等锚点**：路由表仅含 stereoWidth 目标路由（非零 amount/offset），masterGain 恒 1，
   输出与输入逐位一致；输入与 case1 相同（成对对照）（GWT-MM-05）。

维度说明：四条 case 覆盖 LFO sine/saw/square（square 随 case4 参数载荷固化、不进入音频
路径）与双调制源、offset 有/无、钳制双向、多采样率（44100）、非整除块长（末块 112/267/
240/112 帧）。**全部含 masterGain 调制的向量输出依赖 blockSize（GWT-MM-06），两支线必须
按向量固定的 blockSize 回放**。reset 复现性、极值钳制、抛错路径由单元测试覆盖
（GWT-MM-08/09），不入向量。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按本规格
§4.4 Stage 模型执行。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/modulation.ts`（`ModulationMatrix`/`Lfo`/`EnvelopeFollower`）；
  路由/源类型：`src/types.ts`（`ModulationRoute`/`LfoShape`/`ModulationSettings`）
- 引擎接线（块头推进 / `mod-master-gain` 阶段 / `mid-side` 宽度分支 / enabled 门控）：
  `src/engine/HyperSoundEngine.ts`
- 参考单元测试：`test/modulation.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜
  [eq-chain](eq-chain.md) ｜ [fdn-reverb](fdn-reverb.md) ｜ [deesser](deesser.md) ｜
  [loudness-comp](loudness-comp.md) ｜ [dynamic-eq](dynamic-eq.md) ｜ [mod-effects](mod-effects.md) ｜
  [fft](fft.md) ｜ [convolver](convolver.md) ｜ [hse-stretch](hse-stretch.md)
