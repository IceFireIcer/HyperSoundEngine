/**
 * HyperSoundEngine v1 —— 引擎工厂（独立接入入口）
 *
 * 外部软件只需：
 *   import { createEngine } from 'hypersoundengine'
 *   const engine = createEngine(48000, 2)
 *   engine.setParams(params)
 *   engine.process([inL, inR], [outL, outR])
 */

import { HyperSoundEngine } from './HyperSoundEngine'
import type { AudioEngine } from '../interfaces'

/** 按采样率创建独立音频引擎实例（默认立体声） */
export function createEngine(sampleRate: number, channelCount = 2): AudioEngine {
  return new HyperSoundEngine(sampleRate, channelCount)
}

/** 创建引擎并返回具体类型（需要访问 HyperSoundEngine 专有 API 时使用） */
export function createHyperSoundEngine(sampleRate: number, channelCount = 2): HyperSoundEngine {
  return new HyperSoundEngine(sampleRate, channelCount)
}
