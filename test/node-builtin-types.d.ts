/**
 * node-builtin-types.d.ts —— Node 内置模块的最小类型声明
 *
 * 背景：仓库 devDependencies 未包含 @types/node（且 tsconfig 的 types 为空数组），
 * 但对拍门禁测试（test/spec-vectors.test.ts）需要 node:fs / node:path / node:url
 * 读写 specs/dsp/vectors 冻结向量。为不引入新依赖，此处内联该测试用到的最小表面。
 *
 * 维护约定：
 *  - 只声明实际用到的函数；不要扩展成完整 Node 类型表；
 *  - 未来若仓库引入 @types/node，请整体删除本文件，避免声明合并产生歧义。
 */
declare module 'node:fs' {
  export function existsSync(path: string): boolean
  export function readdirSync(path: string): string[]
  export function readFileSync(path: string, encoding: 'utf8'): string
  export function readFileSync(path: string): Uint8Array
}

declare module 'node:path' {
  export function dirname(path: string): string
  export function join(...segments: string[]): string
  export function resolve(...segments: string[]): string
}

declare module 'node:url' {
  export function fileURLToPath(url: string): string
}
