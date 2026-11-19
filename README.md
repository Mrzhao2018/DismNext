# DismNext

DismNext 是面向 Windows 11 的本地系统维护工具。你可以用它找出磁盘空间的去向、预览清理规则会处理哪些文件，也可以管理系统设置，并通过注册表快照撤销已记录的修改。

[![Windows CI](https://github.com/Mrzhao2018/DismNext/actions/workflows/windows-ci.yml/badge.svg?branch=main)](https://github.com/Mrzhao2018/DismNext/actions/workflows/windows-ci.yml)

## 快速开始

### 下载运行

在 [GitHub Releases](https://github.com/Mrzhao2018/DismNext/releases/latest) 下载 Windows x64 版本。

| 文件 | 用途 |
| --- | --- |
| `DismNext-*-win64-portable.exe` | 单文件桌面版，内置界面和规则库，下载后可直接运行。 |
| `dism-cli-*-win64.exe` | 独立命令行和 MCP 入口，在 PowerShell 中运行。 |
| `DismNext-*-win64-bundle.zip` | 包含桌面版、CLI 和 MCP 配置示例的完整压缩包。 |
| `SHA256SUMS.txt` | 校验发布文件的 SHA-256。 |

桌面版启动本地服务，默认地址为 `http://127.0.0.1:9527`，并尝试用 Microsoft Edge 的应用模式打开界面。需要使用 `main` 上的最新功能时，可以从源码构建。

### 从源码构建

准备 Windows、Rust stable（MSVC 工具链）、带 Windows SDK 的 Visual Studio C++ Build Tools，以及 Node.js 22。然后在仓库根目录运行以下命令。

```powershell
Push-Location ui
npm ci --no-audit
npm run build
Pop-Location

cargo build --release --workspace --locked
.\target\release\dism-app.exe
```

先构建前端，让 Rust 构建过程把 `ui/dist` 打包进桌面 EXE。输出位于 `target/release/`，其中 `dism-app.exe` 启动桌面界面，`dism-cli.exe` 提供命令行和 MCP。

如果想自己在浏览器中打开界面，可用 `.\target\release\dism-app.exe --no-window` 启动服务。默认端口被占用时，可用 `--port` 指定一个可用端口。

## 能做什么

| 功能 | 你可以用它做什么 |
| --- | --- |
| 空间回收 | 扫描系统临时文件和应用缓存，按规则预览，查看占用或受保护的文件。支持自定义规则、排除项、Dry Run 和跨规则目标去重。 |
| 磁盘分析 | 按目录、文件类型和大文件查看逻辑大小。可选统计 NTFS 文件数据占用，并对硬链接去重。空间分析、查重与清理规则全量扫描都有进度和取消操作。 |
| 重复文件 | 按内容找出重复文件，选择保留最新副本。回收前重新核对文件，界面逐项显示移入回收站、跳过和失败的结果。 |
| 系统优化 | 查看和应用 Windows 设置，记录相关注册表修改的快照，并从快照历史执行回滚。 |
| 应用与系统管理 | 查看启动项来源和目标签名，管理启动项、服务、计划任务、AppX/MSIX 和 Windows 可选功能。支持 Win32 应用卸载与残留扫描，以及第三方驱动枚举和备份。 |
| 系统急救与报告 | 查看或创建系统还原点，运行 DISM 和 SFC 检查或修复，只读查看离线 WIM 的镜像索引与系统挂载记录，并将健康报告导出为 Markdown 或 HTML。 |

第一次清理时，可以按 **扫描 → 预览 → Dry Run → 确认执行** 的顺序操作。部分系统级清理、驱动和注册表操作需要管理员权限。普通清理规则可能直接删除文件，注册表快照用于恢复其记录的注册表修改。

## 命令行

下面的示例以源码构建出的 `dism-cli.exe` 为例。使用发布版时，把 `$cli` 换成下载后的路径即可。

```powershell
$cli = '.\target\release\dism-cli.exe'
& $cli info
& $cli scan
& $cli preview clean-042
& $cli clean clean-042
& $cli diskmap "$env:TEMP" --depth 2
& $cli diskmap "$env:TEMP" --depth 2 --allocated
& $cli opt list
& $cli snapshot list
```

`clean-042` 是内置的「临时文件」规则。`clean` 默认执行 Dry Run，确认目标后，用 `clean clean-042 --execute` 执行实际清理。用 `--help` 查看所有命令，或用 `drivers --help`、`snapshot --help` 查看子命令。`info`、`scan` 和 `preview` 等命令支持 `--json`。

`diskmap --allocated` 会额外读取 NTFS 文件数据的分配空间，并报告无法测量的文件。目录树、文件类型和大文件排名按逻辑大小计算。

用 `wim "D:\sources\install.wim" --json` 查看离线镜像的索引、版本和架构。Windows 允许时，报告也会列出系统 WIM 挂载记录。

## MCP 接入

支持 stdio 的 MCP 客户端可以直接启动 `dism-cli.exe mcp`。以下示例使用 `mcpServers` 配置格式，请把 `command` 换成你机器上的绝对路径。

```json
{
  "mcpServers": {
    "dismnext": {
      "command": "C:/Tools/DismNext/dism-cli.exe",
      "args": ["mcp"]
    }
  }
}
```

客户端可获取系统概况、扫描规则、分析磁盘、查找重复文件和生成健康报告。清理工具 `dismnext_execute_cleanup` 默认使用 `dry_run: true`。

## 项目结构与验证

| 路径 | 职责 |
| --- | --- |
| [`crates/dism-core`](crates/dism-core) | Rust 核心引擎，负责规则解析、扫描、清理、快照和 Windows 系统操作。 |
| [`crates/dism-cli`](crates/dism-cli) | CLI 和 stdio MCP 服务。 |
| [`crates/dism-app`](crates/dism-app) | 本地 HTTP 服务、后台扫描任务和桌面启动器。 |
| [`ui`](ui) | 使用 React、TypeScript、Fluent UI 9 和 Vite 构建的中英双语界面。 |
| [`catalog/rules.json`](catalog/rules.json) | 构建时嵌入的清理与优化规则。 |

桌面版和 CLI 将注册表快照保存在运行时工作目录下的 `data/snapshots/`。自定义清理规则保存在 `data/custom_rules.json`，应用会在工作目录和 EXE 所在目录查找该文件。

完成前端和发布构建后，可在仓库根目录运行本地验证。

```powershell
cargo test --workspace --locked
.\scripts\check_portable_ui.ps1 -Executable .\target\release\dism-app.exe
```

[Windows CI](.github/workflows/windows-ci.yml) 在推送和 Pull Request 时执行前端构建、Rust 测试、发布构建和单文件 EXE 冒烟检查。启动检查会在临时目录中运行单独的桌面 EXE，并验证它能提供内嵌页面、JavaScript 和规则库。
