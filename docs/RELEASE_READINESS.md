# 1.5.1 正式发布就绪度

> 审计日期：2026-08-31
>
> 审计提交：`ad860db`
>
> 自动门禁证据：[CI run 33365076659](https://github.com/IceFireIcer/HyperSoundEngine/actions/runs/33365076659)，`test` / `rust` / `rust-windows-silent` 全部成功。
>
> 本文区分 GitHub 源码 Release、npm 包、Windows 二进制和 crates.io 四种发行渠道。

## 结论

| 发布渠道 | 当前判断 | 原因 |
|---|---|---|
| GitHub 源码 Release | **有条件可发布** | 1.5.1 版本一致、源码完整、自动 CI 全绿；Release notes 必须明确外部验收缺口 |
| npm `hypersoundengine` | **暂不发布** | tarball 边界和 wasm 资产交付尚未收口，也没有 registry 发布凭据/所有权证据 |
| Windows 可执行文件 | **暂不作为正式发行物** | 能构建，但缺 Windows 专用发布作业、签名、校验和、运行时/安装说明和真机验收 |
| crates.io | **不可发布** | workspace crates 明确 `publish=false`，内部 path 依赖也未配置发布版本 |

因此，“发布正式版”如果指 **GitHub 上的源码归档**，可以创建 `v1.5.1` 正式 Release，但必须注明限制。如果指可直接安装的 npm 包、签名 Windows 程序或 crates.io 包，目前答案是否定的。

## 已满足的源码 Release 条件

- npm、package-lock 和七个 Rust workspace 包版本一致为 1.5.1。
- npm 1.5.1 tarball dry-run 已成功生成，大小约 574 KB、解包约 1.88 MB；但内容边界仍有下述阻断项。
- `CHANGELOG.md` 已包含 1.5.1 修复与接入文档记录。
- `main` 自动门禁覆盖 TS、Rust、Windows silent、wasm32、正式 Chromium AudioWorklet 和共享规格对拍。
- 现有冻结音频向量保持不变：72/72；空间 28/28；参数扫描结构摘要 40/40。
- LICENSE 与 npm/Cargo SPDX 均为 `CC-BY-NC-ND-4.0`。
- README 已明确署名、非商业和禁止分发修改版本的核心限制。

## GitHub 源码 Release 的发布条件

发布前应确认：

1. `main` 当前提交的三个 CI job 全绿。
2. 创建 `v1.5.1` tag，并确保 tag 指向已经验证的提交。
3. Release 标记为正式版而非 pre-release，仅附 GitHub 自动源码归档或人工确认过的源码包。
4. Release notes 明确：
   - Phase 4 自动实现完成，但真实 shared/exclusive 设备延迟、xrun 和整进程 CPU 尚未验收；
   - Phase 5 主体实现完成，但真实 SOFA 自动门禁和 Firefox E2E 尚未完成；
   - 物理 multichannel 输出尚未实现，当前最终输出为双耳立体声；
   - `hse-napi` 未实现；跨语言完整 Rust 引擎使用 `hse-service`；
   - 许可为 CC BY-NC-ND 4.0。

当前仓库没有 tag 驱动的正式 release workflow，只有每日 pre-release 工作流。因此正式 Release 需要人工创建，或先新增单独的正式发布 workflow。

## npm 发布阻断项

当前 `npm pack --dry-run` 能构建 1.5.1 tarball，但还不能视为稳定公开包：

- `package.json.files` 包含 `adapters/`；其中 WaveForge 适配源码引用仓库内 `src/`、`ui/` 和宿主自有依赖，打包后不能作为独立入口使用。
- `prepack` 当前不清理 `dist/`；本次本地 tarball 清单实际包含残留的 `dist/debug-worklet.js`，证明发布内容会受工作区旧构建产物影响。
- TS worklet bundle 在 `dist/` 中，但没有独立公开 export；Rust wasm worklet 和 `.wasm` 也未形成 npm 资产交付约定。
- 缺少 `repository`、`bugs`、`homepage`、`engines` 和 `publishConfig` 等正式包元数据。
- 当前环境没有 npm 发布凭据，也没有已确认的包名所有权或 trusted publishing 配置。

完成这些事项并在干净 checkout 上验证 tarball 安装、导入和浏览器资产后，才建议执行 `npm publish`。

## Windows 二进制发布阻断项

当前 nightly 在 Linux 构建 Rust，不能产出可用的 Windows WASAPI 正式包。正式 Windows 发行还需要：

- Windows release runner 构建 `hse-service.exe`、`hse-cli.exe` 和相关工具。
- 明确 MSVC/UCRT 运行时前置条件或静态链接策略。
- 生成平台/架构明确的压缩包和 SHA-256 清单。
- 汇总完整运行时第三方许可证。
- 配置 Authenticode 代码签名及可信时间戳。
- 完成 shared/exclusive 真机延迟、xrun 和 CPU 验收。
- 提供安装、升级、卸载和服务启动说明。

在这些条件完成前，可以提供开发者自构建说明，不应把未签名裸 EXE 称为正式 Windows Release。

## crates.io 发布阻断项

当前所有 Rust crate 都设置 `publish = false`。如未来决定公开 crates，需要另行确定：

- 哪些 crate 是稳定公共 API；
- 内部依赖的 `version + path`；
- crate 发布顺序；
- README、keywords、categories、rust-version 和许可证文件；
- 每个 crate 的 `cargo package` / 解包构建测试；
- crates.io 凭据与所有权。

当前不要执行 `cargo publish`。

## 外部产品验收仍未完成

这些项目不阻止发布明确标注限制的源码 Release，但阻止宣称完整 Windows 产品或完整空间音频产品已经正式验收：

- shared 完整服务链 p95 ≤30ms、零 xrun、整进程 CPU；
- exclusive capture→DSP→render p95 ≤10ms、零 xrun、整进程 CPU；
- 正式播放器/VB-CABLE/真实非零 capture 与双推流联合验证；
- 许可可再分发的真实 SOFA 数据集矩阵；
- Firefox AudioWorklet E2E；
- 物理多声道输出。

真实音频仍必须同时设置 `HSE_ALLOW_REAL_AUDIO=1` 并传 `--run`；不得用 fake、silent CI 或服务排队帧统计替代设备端到端验收。
