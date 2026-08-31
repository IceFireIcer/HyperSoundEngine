import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import { createDefaultParams } from '../src/types'
import { HyperSoundEngine } from '../src/engine/HyperSoundEngine'

class FakeAudioWorkletProcessor {
  readonly port = {
    onmessage: null as ((event: MessageEvent) => void) | null,
    postMessage: vi.fn((_message: unknown) => undefined),
  }

  constructor(_options?: unknown) {}
}

let Processor: typeof import('../src/worklet/HseAudioEffectsProcessor').HseAudioEffectsProcessor

beforeAll(async () => {
  vi.stubGlobal('AudioWorkletProcessor', FakeAudioWorkletProcessor)
  vi.stubGlobal('sampleRate', 48000)
  vi.stubGlobal('registerProcessor', vi.fn())
  ;({ HseAudioEffectsProcessor: Processor } = await import('../src/worklet/HseAudioEffectsProcessor'))
})

afterAll(() => {
  vi.unstubAllGlobals()
})

describe('HseAudioEffectsProcessor 参数初始化', () => {
  it('构造期应用 initialParams、发送 ready，运行期忽略 params 重建消息', () => {
    const setParams = vi.spyOn(HyperSoundEngine.prototype, 'setParams')
    const params = createDefaultParams(48000)
    params.stereoWidth = 0.5
    const processor = new Processor({
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      parameterData: {},
      processorOptions: { inputChannelCount: 2, initialParams: params, requestId: 'ready-1' },
    })

    expect(setParams).toHaveBeenCalledWith(params)
    expect(processor.port.postMessage).toHaveBeenCalledWith({ type: 'ready', requestId: 'ready-1' })
    setParams.mockClear()
    processor.port.onmessage?.({ data: { type: 'params', params: createDefaultParams(48000) } } as MessageEvent)
    expect(setParams).not.toHaveBeenCalled()
    processor.port.onmessage?.({ data: { type: 'reset' } } as MessageEvent)
    setParams.mockRestore()
  })

  it('构造期参数失败立即回传关联错误而不发送 ready', () => {
    const originalSetParams = HyperSoundEngine.prototype.setParams
    const setParams = vi.spyOn(HyperSoundEngine.prototype, 'setParams')
      .mockImplementationOnce(function (this: HyperSoundEngine, params) {
        return originalSetParams.call(this, params)
      })
      .mockImplementationOnce(() => {
        throw new Error('invalid params')
      })
    const processor = new Processor({
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      parameterData: {},
      processorOptions: { initialParams: createDefaultParams(48000), requestId: 'ready-error' },
    })

    expect(processor.port.postMessage).toHaveBeenCalledWith({
      type: 'error',
      phase: 'construct',
      requestId: 'ready-error',
      message: 'invalid params',
    })
    expect(processor.port.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ type: 'ready' }))
    setParams.mockRestore()
  })
})

describe('HseAudioEffectsProcessor 多声道输入', () => {
  for (const inputChannelCount of [6, 8] as const) {
    it(`${inputChannelCount} 路输入走 processMulti 且只写双声道输出`, () => {
      const processMulti = vi.spyOn(HyperSoundEngine.prototype, 'processMulti')
      const processor = new Processor({
        numberOfInputs: 1,
        numberOfOutputs: 1,
        outputChannelCount: [2],
        parameterData: {},
        processorOptions: { inputChannelCount },
      })
      const inputs = [Array.from({ length: inputChannelCount }, () => new Float32Array(128))]
      inputs[0][inputChannelCount - 1][0] = 0.5
      const outputs = [[new Float32Array(128), new Float32Array(128)]]

      expect(processor.process(inputs, outputs, {})).toBe(true)
      expect(processMulti).toHaveBeenCalledTimes(1)
      expect(processMulti.mock.calls[0][0]).toHaveLength(inputChannelCount)
      expect(processMulti.mock.calls[0][1]).toHaveLength(2)
      expect(outputs[0][0].every(Number.isFinite)).toBe(true)
      expect(outputs[0][1].every(Number.isFinite)).toBe(true)
      processMulti.mockRestore()
    })
  }
})
