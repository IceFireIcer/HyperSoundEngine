# HyperSoundEngine 调音室 UI 指南

> 本文档讲解 HyperSoundEngine 调音室 UI（`ui/` 目录）的结构、设计语言、组件与接入方式。

## 0. UI 定位与设计语言

HyperSoundEngine 调音室 UI 采用 **liquid glass 玻璃拟态**设计语言：

- **玻璃拟态**：`ui/theme.ts` 中的 `glassPanel / glassCard / glassBorder / glassBlur(30px saturate 185%) / glassCardBlur / glassPanelHighlight`
  （暗色 `rgba(10,12,20,0.38)`、亮色 `rgba(255,255,255,0.45)`）；
- **全局主题色**：`useAccentColor` 监听 `accentColorChanged` 事件 + localStorage `accentColor`（默认 #8b5cf6），与全局联动；
- **交互基元**：胶囊开关（accent 背景 + 发光阴影）、`wf-glass-range` 滑块（白点 thumb + accent 光晕）、
  玻璃卡片、胶囊 Tab、chip 场景、主/幽灵按钮；
- **图标**：lucide-react；**动画**：CSS keyframes（零动画库依赖）。
- 文案与注释均为中文，与全项目一致。

## 1. 目录与依赖

```
HyperSoundEngine/ui/
├── theme.ts              # 设计语言变量（useHyperSoundEngineTheme）
├── primitives.tsx        # Toggle/Slider/GlassCard/Modal/Segmented/Chip/TextInput/ActionButton/InfoLine
├── hooks.ts              # useHyperSoundEngineParams（快照 patch/replace）+ DeepPartial + deepMerge
├── bridge.ts             # HyperSoundEngineUiBridge 接口 + createHyperSoundEngineUiBridge(engine, sampleRate)
├── effectsPanel.tsx      # 音效场景页：场景栏 + 12 效果卡 + 响度卡片
├── modalsSpatial.tsx     # 混响（双路由+IR 导入）/ 3D 环绕（圆形拖拽）/ 低音增强（4 谐波）
├── modalsDynamics.tsx    # 压缩 / 齿音 / 夜间 / 限幅 / IEQ / 变速变调 / 立体声宽度
├── modalsLoudness.tsx    # 音量自适应补偿（auto 曲线可视化）/ 响度归一化
├── eqCurveEditor.tsx     # SVG 对数频率轴曲线编辑器（拖拽控制点）
├── eqPanel.tsx           # 均衡器页：simple/pro、10/20 段、Q 补偿、锁定、预设、导入导出
├── sharePanel.tsx        # 调音器页：分享串（引擎编解码）、WAV 导出、引擎信息
├── analysisPanel.tsx     # 分析页：LUFS/GR/频谱/特征 + 听力测试流程
├── HyperSoundEngineMixingStudio.tsx    # 主面板组装（4 个页签 + 弹窗调度）
└── index.ts              # 公共出口
```

**依赖**：`react`（peer）+ `lucide-react`。本地验证用 `npm run typecheck:ui`（tsconfig.ui.json，jsx react-jsx）。

## 2. 接入方式

调音室 UI 是纯受控组件，只依赖 `HyperSoundEngineUiBridge` 接口与参数快照，不直接 import 引擎核心。

接入步骤：

1. 拷贝 `ui/` 目录到宿主项目；
2. 用引擎实例创建桥：`const bridge = createHyperSoundEngineUiBridge(engine, sampleRate)`；
3. 渲染主面板：

```tsx
import { HyperSoundEngineMixingStudio } from './ui'

const bridge = createHyperSoundEngineUiBridge(engine, ctx.sampleRate)

<HyperSoundEngineMixingStudio
  bridge={bridge}
  onClose={() => setShowStudio(false)}
  playerTheme={playerTheme}
  anchorRect={anchorRect}
  exportWav={exportWav}    // 可选：离线导出
/>
```

- `bridge` 需要稳定引用（useMemo/useRef），引擎重建后重新建桥。

## 3. 宿主接线（可选能力）

| 能力 | 事件/接口 | 宿主侧实现 |
|---|---|---|
| **听力测试播放** | 监听 `hseHearingPlay` 自定义事件：`{ freqHz, levelDb }` | Web Audio 合成正弦（如 OscillatorNode），电平按 `10^(levelDb/20)` 换算幅度；播放时长约 0.6s 后停止，或由下一次事件/用户作答停止 |
| **系统音量 → 补偿曲线** | 写 `loudnessCompensation.volumePercent`（0-100） | 监听系统音量，变化时 `bridge.setParams` 更新；无音量源时默认 80 |
| **WAV 离线导出** | `exportWav` prop | 解码音频 → `HyperSoundEngine.process` 分块 → 写 WAV |

> 听力测试的"播放"不在 ui/ 内实现（纯 UI 不触碰 Web Audio），事件化解耦；未接线时流程 UI 仍可走完（不发声）。

## 4. 页签与功能对照

| 页签 | 内容 | 引擎特有 |
|---|---|---|
| 音效场景 | 12 场景 chips + 12 效果卡（可叠加）+ 音量自适应补偿/响度归一化独立卡 + 我的场景（上限 8） | 齿音/IEQ/限幅/变速/宽度卡片；混响多路由 |
| 均衡器 | simple 5 段 / pro 10-20 段 + **曲线编辑器拖拽** + 级联 Q 补偿 + 锁定 + 预设 + EQ JSON 导入导出 | 20 段、Q 补偿、锁定 |
| 调音器 | **引擎分享串**（完整参数，版本+校验+白名单）+ WAV 导出 + 引擎信息（采样率/延迟/LUFS/GR） | 分享串格式 |
| 分析 | LUFS/LRA/峰值/真峰值 + 限幅 GR 条 + 32 条频谱 + 5 项特征 + 听力测试（7 频点 × 5 轮） | 全部 |

## 5. 设计说明

1. **UI 与引擎解耦**：所有面板只依赖 `HyperSoundEngineUiBridge` 接口与参数快照，不 import HyperSoundEngine；
   宿主可替换桥实现（如包一层 Web Audio 适配）。
2. **快照语义**：`useHyperSoundEngineParams` 的 patch 做深合并后整包提交（`setParams` 完整快照），符合引擎契约；
   场景/分享串/恢复默认走 replace。
3. **零动画依赖**：CSS keyframes（`hse-panel-in` 支持锚点偏移变量 `--fx/--fy`）。
4. **测试策略**：ui/ 为纯受控组件 + 桥接口；
   本地保证：`npm run typecheck:ui` 0 错误；引擎回归 + UI 冒烟（9）全绿。
