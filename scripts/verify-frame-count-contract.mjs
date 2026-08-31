#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import Ajv from 'ajv'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const fixturePath = path.join(repoRoot, 'specs', 'engine', 'vectors', 'frame-count.v1.json')
const schemaPath = path.join(repoRoot, 'specs', 'schema', 'frame-count.schema.json')

for (const required of [schemaPath, fixturePath]) {
  if (!existsSync(required)) throw new Error('frameCount 门禁文件缺失：' + required)
}

const schema = JSON.parse(readFileSync(schemaPath, 'utf8'))
const fixture = JSON.parse(readFileSync(fixturePath, 'utf8'))
const validate = new Ajv({ allErrors: true }).compile(schema)
if (!validate(fixture)) {
  throw new Error('frameCount 夹具不符合 Schema：' + JSON.stringify(validate.errors))
}
for (const testCase of fixture.cases) {
  if (testCase.blockFrames.length !== testCase.seeds.length) {
    throw new Error(testCase.id + ': blockFrames 与 seeds 长度必须一致')
  }
  if (!testCase.blockFrames.some((frames) => frames < testCase.preparedCapacity)) {
    throw new Error(testCase.id + ': 必须包含短于 preparedCapacity 的块')
  }
  if (testCase.blockFrames.some((frames) => frames > testCase.preparedCapacity)) {
    throw new Error(testCase.id + ': 块长不得超过 preparedCapacity')
  }
}
console.log('frameCount 共享夹具验证通过：' + fixture.cases.length + ' case')
