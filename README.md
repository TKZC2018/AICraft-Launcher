<div align="center">

<img src="logo.png" width="120" alt="AICraft Launcher">

# AICraft Launcher

基于 Rust 的开源 Minecraft: Java Edition 启动器。
**零运行时依赖**，不装 Java 以外的任何东西，下载完就能玩。

</div>

---

## 这是什么

AICraft 是一个轻量的 MCJE 启动器。

> **状态：测试阶段，尚未正式发布。**

## 功能

市面上 Minecraft 启动器的大部分功能，以及一些特色功能。

## 快速开始

下载 `AICraftLauncher.exe`，双击，选个游戏目录，点「下载版本」挑一个装上就行。

### 从源码构建

需要 MSVC 工具链，以及：

- 核心库与命令行：Rust **1.75+**（零外部 crate）
- 图形界面：Rust **1.95+**（eframe / egui 0.37 自身的要求）

```bash
# 核心库与命令行（零外部 crate，离线可构建）
cargo build --release -p aicraft-core
cargo build --release -p aicraft-cli

# 图形界面（独立的 workspace，必须用 msvc 工具链）
cd crates/aicraft-ui
cargo build --release
```

产物：`crates/aicraft-ui/target/release/AICraftLauncher.exe`。

正版登录需要一份 Azure 应用的客户端 ID —— 它**不在源码里**，由编译期注入。任选一种方式提供：

```bash
# 1. 环境变量
AICRAFT_CLIENT_ID=你的应用ID cargo build --release -p aicraft-core

# 2. 仓库根放一个 .client-id 文件（已在 .gitignore 里）
echo 你的应用ID > .client-id
```

两条都没有时编译照常通过，只是正版登录会直接提示「这一份构建没有带 Azure 应用 ID」。离线模式不受影响。

## 仓库结构

```
crates/
  aicraft-core/   核心逻辑：清单解析、下载、启动、加载器、整合包、服务端……
  aicraft-ui/     图形界面（eframe/egui）。独立 workspace，见下方说明
  aicraft-cli/    命令行工具 `aicraft`
```

**为什么 UI 不在根 workspace 里**：workspace 的依赖解析是全局的，只要有一个成员引用 `eframe`，`cargo build -p aicraft-core --offline` 也会因为找不到 `eframe` 而失败。把界面拆出去之后，core 和 cli 保持「零第三方依赖、离线可构建」——这是本项目最重要的工程约束。

## 许可证

[GPL-3.0-or-later](LICENSE)

---

> 本项目代码由 AI 生成。
