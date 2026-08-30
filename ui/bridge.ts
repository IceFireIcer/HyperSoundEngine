/**
 * HyperSoundEngine v1 调音室 UI —— 引擎桥（HyperSoundEngineUiBridge）
 *
 * UI 只依赖本文件的桥接口（不直接 import HyperSoundEngine），融合时把桥接实现换到
 * HyperSoundEngine 侧（引擎实例来自 HyperSoundEngineHost.engine 或直接 new HyperSoundEngine）即可。
 *
 * 桥职责：
 *  - 参数快照读写（setParams 每次收完整快照）；
 *  - 引擎统计/分析读取（LUFS、频谱、特征）；
 *  - 场景：内置 11 场景 + 我的场景（localStorage 持久化，快照去 IR 数据）；
 *  - 分享串：encode/decode（版本+校验+白名单，非法输入抛错）；
 *  - 听力测试状态机（HseHearingTest 封装）。
 */

import type { EngineAnalysis, EngineStats, ScenePreset, HyperSoundEngineParams } from '../src/types'
import { createDefaultParams } from '../src/types'
import type { AudioEngine } from '../src/interfaces'
import { SCENE_PRESETS, getSceneById } from '../src/engine/ScenePresets'
import { encodeShareCode, decodeShareCode } from '../src/engine/ShareCodec'
import { HseHearingTest, type AudiogramPoint } from '../src/analysis/HseHearingTest'

/** 我的场景存储键（独立命名空间） */
const MY_SCENES_KEY = 'hypersound:hse-my-scenes'
/** 我的场景上限 */
export const MAX_MY_SCENES = 8

export interface HyperSoundEngineHearingSession {
  /** 当前待测步骤；null=未开始或已完成 */
  step: { freqHz: number; levelDb: number } | null
  /** 进度：当前频点序号（0-6）/ 频点内轮数（0-4），共 7 频点 × 5 轮 */
  freqIndex: number
  round: number
  done: boolean
  audiogram: AudiogramPoint[]
}

export interface HyperSoundEngineUiBridge {
  /** 当前参数快照（深拷贝，防止外部突变） */
  getParams(): HyperSoundEngineParams
  /** 设置完整快照（引擎 setParams；UI 侧始终传 getParams 深拷贝修改后的版本） */
  setParams(p: HyperSoundEngineParams): void
  getStats(): EngineStats
  getAnalysis(): EngineAnalysis
  getLatencySamples(): number
  getSampleRate(): number
  /** 内置 11 场景 + 我的场景 */
  getScenes(): ScenePreset[]
  applyScene(id: string): void
  saveMyScene(name: string): boolean
  deleteMyScene(id: string): void
  /** 导出分享串（完整参数快照，含版本+校验） */
  encodeShare(p: HyperSoundEngineParams): string
  /** 解析分享串；非法输入抛 Error */
  decodeShare(code: string): HyperSoundEngineParams
  /** 听力测试 */
  beginHearing(): void
  hearingStep(): HyperSoundEngineHearingSession
  answerHearing(heard: boolean): HyperSoundEngineHearingSession
  resetHearing(): void
}

/** 快照入库前去除不可序列化数据（卷积 IR 数组 → irName 引用语义） */
function sanitizeForStorage(p: HyperSoundEngineParams): HyperSoundEngineParams {
  const clone = JSON.parse(JSON.stringify(p)) as HyperSoundEngineParams
  clone.reverb.convolution.ir = null
  return clone
}

function loadMyScenes(): ScenePreset[] {
  try {
    const raw = localStorage.getItem(MY_SCENES_KEY)
    if (!raw) return []
    const list = JSON.parse(raw) as ScenePreset[]
    return Array.isArray(list) ? list.filter((s) => s && typeof s.id === 'string') : []
  } catch {
    return []
  }
}

function saveMyScenes(list: ScenePreset[]): void {
  try {
    localStorage.setItem(MY_SCENES_KEY, JSON.stringify(list))
  } catch {
    // 存储不可用时静默（不影响播放）
  }
}

/** 把任意 AudioEngine 包装成 UI 桥（融合时在 HyperSoundEngine 侧调用） */
export function createHyperSoundEngineUiBridge(engine: AudioEngine, sampleRate: number): HyperSoundEngineUiBridge {
  const hearing = new HseHearingTest(sampleRate)
  let current: HyperSoundEngineParams = createDefaultParams(sampleRate)
  engine.setParams(current)

  const readHearing = (): HyperSoundEngineHearingSession => {
    const step = hearing.nextStep()
    return {
      step,
      freqIndex: hearing.getFreqIndex(),
      round: hearing.getRound(),
      done: hearing.isDone(),
      audiogram: hearing.getAudiogram(),
    }
  }

  const impl: HyperSoundEngineUiBridge = {
    getParams: () => JSON.parse(JSON.stringify(current)) as HyperSoundEngineParams,
    setParams: (p: HyperSoundEngineParams) => {
      current = JSON.parse(JSON.stringify(p)) as HyperSoundEngineParams
      engine.setParams(current)
    },
    getStats: () => engine.getStats(),
    getAnalysis: () => engine.getAnalysis(),
    getLatencySamples: () => engine.getLatencySamples(),
    getSampleRate: () => sampleRate,
    getScenes: () => [...SCENE_PRESETS, ...loadMyScenes()],
    applyScene: (id: string) => {
      const scene = getSceneById(id) ?? loadMyScenes().find((s) => s.id === id)
      if (!scene) return
      impl.setParams(scene.params)
    },
    saveMyScene: (name: string): boolean => {
      const mine = loadMyScenes()
      if (mine.length >= MAX_MY_SCENES) return false
      const snapshot = sanitizeForStorage(current)
      snapshot.sceneId = null
      snapshot.customized = true
      const scene: ScenePreset = {
        id: `my-${Date.now()}-${Math.floor(Math.random() * 1e6)}`,
        name,
        builtin: false,
        params: snapshot,
      }
      mine.push(scene)
      saveMyScenes(mine)
      return true
    },
    deleteMyScene: (id: string) => {
      saveMyScenes(loadMyScenes().filter((s) => s.id !== id))
    },
    encodeShare: (p: HyperSoundEngineParams) => encodeShareCode(p),
    decodeShare: (code: string) => decodeShareCode(code),
    beginHearing: () => hearing.begin(),
    hearingStep: () => readHearing(),
    answerHearing: (heard: boolean) => {
      hearing.answer(heard)
      return readHearing()
    },
    resetHearing: () => hearing.reset(),
  }
  return impl
}