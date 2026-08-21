/**
 * vitest 配置：esbuild 使用 automatic JSX（与 tsconfig.ui.json 的 react-jsx 一致）。
 * 仅影响 .tsx（ui/ 冒烟测试）；src/ 纯 TS 测试不受影响。
 * jsdom 环境由测试文件头注释（@vitest-environment jsdom）按文件启用。
 */
import { defineConfig } from 'vitest/config'

export default defineConfig({
  esbuild: {
    jsx: 'automatic',
  },
})
