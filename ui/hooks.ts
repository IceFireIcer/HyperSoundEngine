/**
 * HyperSoundEngine v1 调音室 UI —— 参数快照 hooks
 *
 * 参数语义（引擎约定）：完整快照（HyperSoundEngineParams），setParams 每次替换整包。
 * 本 hook 提供：
 *  - params：当前快照（深拷贝展示值）
 *  - patch(partial)：深合并后提交（UI 局部修改的惯用入口）
 *  - replace(next)：整包替换（场景应用 / 分享串导入 / 恢复默认）
 */

import { useCallback, useState } from 'react'
import type { HyperSoundEngineParams } from '../src/types'
import type { HyperSoundEngineUiBridge } from './bridge'

/** 递归可选（数组与 Float32Array 整体替换，不做成员递归） */
export type DeepPartial<T> = {
  [K in keyof T]?: T[K] extends Float32Array | Array<unknown> ? T[K] : T[K] extends object ? DeepPartial<T[K]> : T[K]
}

/** 深合并：普通对象递归；数组/原始值/Float32Array 直接替换 */
export function deepMerge<T>(base: T, patch: DeepPartial<T>): T {
  if (patch === null || typeof patch !== 'object' || Array.isArray(patch) || patch instanceof Float32Array) {
    return patch as T
  }
  if (base === null || typeof base !== 'object' || Array.isArray(base) || base instanceof Float32Array) {
    return patch as T
  }
  const out: Record<string, unknown> = { ...(base as Record<string, unknown>) }
  for (const key of Object.keys(patch as Record<string, unknown>)) {
    const pv = (patch as Record<string, unknown>)[key]
    const bv = (base as Record<string, unknown>)[key]
    out[key] = deepMerge(bv as never, pv as never)
  }
  return out as T
}

export interface HyperSoundEngineParamsController {
  params: HyperSoundEngineParams
  /** 深合并局部修改并提交引擎（完整快照语义） */
  patch: (partial: DeepPartial<HyperSoundEngineParams>) => void
  /** 整包替换（场景/分享串/恢复默认） */
  replace: (next: HyperSoundEngineParams) => void
}

export function useHyperSoundEngineParams(bridge: HyperSoundEngineUiBridge): HyperSoundEngineParamsController {
  const [params, setParams] = useState<HyperSoundEngineParams>(() => bridge.getParams())

  const commit = useCallback((next: HyperSoundEngineParams) => {
    bridge.setParams(next)
    setParams(bridge.getParams())
  }, [bridge])

  const patch = useCallback((partial: DeepPartial<HyperSoundEngineParams>) => {
    commit(deepMerge(bridge.getParams(), partial))
  }, [bridge, commit])

  const replace = useCallback((next: HyperSoundEngineParams) => {
    commit(next)
  }, [commit])

  return { params, patch, replace }
}
