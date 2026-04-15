# 实验报告：exercise-printcolor

## 1. 实验名称

**exercise-printcolor** —— ArceOS 彩色输出练习

---

## 2. 实验目的

本实验旨在通过在 ArceOS unikernel 框架中实现带 ANSI 颜色转义序列的文本输出，达成以下学习目标：

1. **理解 ArceOS unikernel 框架**：掌握 ArceOS 的项目结构、构建系统及多架构支持机制，了解从应用层到硬件抽象层的完整依赖链。
2. **掌握 ANSI 转义序列**：学习 ANSI SGR（Select Graphic Rendition）控制序列的格式与用法，理解如何在终端中实现彩色文本输出。
3. **裸机环境下的输出机制**：理解在 `no_std`、`no_main` 的裸机环境中，如何通过 `axstd` 标准库将输出请求逐层传递，最终经由硬件 UART 完成字符输出。
4. **跨架构构建与验证**：熟悉使用 xtask 工具进行多架构（riscv64、aarch64、x86_64、loongarch64）的构建与 QEMU 模拟运行。

---

## 3. 代码框架分析

### 3.1 项目结构

```
exercise-printcolor/
├── src/main.rs              # 应用入口
├── build.rs                 # 构建脚本（链接器配置）
├── Cargo.toml               # 项目清单与依赖
├── rust-toolchain.toml      # Rust 工具链配置
├── configs/                 # 各架构平台配置
│   ├── riscv64.toml
│   ├── aarch64.toml
│   ├── x86_64.toml
│   └── loongarch64.toml
├── xtask/src/main.rs        # 构建/运行编排工具
├── scripts/test.sh          # 多架构验证脚本
└── .cargo/                  # Cargo 配置
```

### 3.2 核心文件分析

#### [`src/main.rs`](src/main.rs) —— 应用入口

```rust
#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
use axstd::println;

#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
fn main() {
    println!("\x1b[32m[WithColor]: Hello, Arceos!\x1b[0m");
}
```

- `#![cfg_attr(feature = "axstd", no_std)]`：当启用 `axstd` 特性时，禁用标准库，使用 `no_std` 环境。
- `#![cfg_attr(feature = "axstd", no_main)]`：当启用 `axstd` 特性时，禁用标准入口点，由 ArceOS 运行时提供入口。
- `use axstd::println`：从 ArceOS 标准库引入 `println!` 宏，用于串口输出。
- `unsafe(no_mangle)`：确保 `main` 函数符号名不被修改，以便 ArceOS 运行时能够找到并调用它。

#### [`Cargo.toml`](Cargo.toml) —— 项目清单

```toml
[package]
name = "arceos-printcolor"
version = "0.4.1"
edition = "2024"

[features]
default = ["axstd"]
axstd = ["dep:axstd"]
xtask = ["dep:clap"]

[dependencies]
axstd = { version = "=0.3.0-preview.1", features = ["defplat"], optional = true }
clap = { version = "4", features = ["derive"], optional = true }
```

关键依赖说明：

| 依赖 | 版本 | 说明 |
|------|------|------|
| `axstd` | `=0.3.0-preview.1` | ArceOS 标准库，提供 `println!` 等宏；`defplat` 特性用于默认平台选择 |
| `clap` | `4` | 命令行参数解析库，仅用于 xtask 工具 |

#### [`build.rs`](build.rs) —— 构建脚本

[`build.rs`](build.rs) 仅在目标为 `*-none-*` 裸机平台时激活，主要完成以下工作：

1. **架构自动检测**：通过 `CARGO_CFG_TARGET_ARCH` 环境变量获取目标架构，映射到对应平台名称：

   | 架构 | 平台 |
   |------|------|
   | `riscv64` | `riscv64-qemu-virt` |
   | `aarch64` | `aarch64-qemu-virt` |
   | `x86_64` | `x86-pc` |
   | `loongarch64` | `loongarch64-qemu-virt` |

2. **链接器脚本定位**：从 `OUT_DIR` 回溯至 `target/{triple}/{profile}/` 目录，找到由 `axhal` 生成的链接器脚本 `linker_{platform}.lds`。

3. **传递链接器参数**：
   - `-T{linker_script}`：指定链接器脚本
   - `-no-pie`：禁用位置无关可执行文件
   - `-znostart-stop-gc`：防止链接器垃圾回收回收启动/停止符号

#### [`xtask/src/main.rs`](xtask/src/main.rs) —— 构建/运行编排工具

xtask 是一个基于 `clap` 的命令行工具，提供 `build` 和 `run` 两个子命令，支持 `--arch` 参数指定目标架构。其工作流程如下：

1. **安装配置**（[`install_config()`](xtask/src/main.rs:78)）：将 `configs/{arch}.toml` 复制为 `.axconfig.toml`，供 `axconfig` 组件读取。
2. **编译构建**（[`do_build()`](xtask/src/main.rs:98)）：执行 `cargo build --release --target {triple}`，并通过 `AX_CONFIG_PATH` 环境变量确保依赖读取正确的配置。
3. **格式转换**（[`do_objcopy()`](xtask/src/main.rs:121)）：对非 x86_64 架构，使用 `rust-objcopy` 将 ELF 转换为原始二进制文件（`-O binary --strip-all`）。
4. **QEMU 运行**（[`do_run_qemu()`](xtask/src/main.rs:140)）：根据架构选择不同的 QEMU 启动参数：

   | 架构 | QEMU 命令 | 机器类型 | 内核格式 | 特殊参数 |
   |------|-----------|----------|----------|----------|
   | riscv64 | `qemu-system-riscv64` | virt | raw binary | `-bios default` |
   | aarch64 | `qemu-system-aarch64` | virt | raw binary | `-cpu cortex-a72` |
   | x86_64 | `qemu-system-x86_64` | q35 | ELF | 无需 objcopy |
   | loongarch64 | `qemu-system-loongarch64` | virt | raw binary | — |

#### [`configs/`](configs/) —— 架构配置文件

每个架构对应一个 TOML 配置文件，定义了平台的关键参数。以 [`configs/riscv64.toml`](configs/riscv64.toml) 为例：

```toml
arch = "riscv64"
package = "axplat-riscv64-qemu-virt"
platform = "riscv64-qemu-virt"
task-stack-size = 0x40000
ticks-per-sec = 100

[devices]
mmio-ranges = [[0x0010_1000, 0x1000], [0x0c00_0000, 0x21_0000], ...]
plic-paddr = 0x0c00_0000
uart-type = "ns16550a"
uart-paddr = 0x1000_0000
```

配置内容包括：架构标识、平台包名、任务栈大小、时钟频率、设备规格（UART 类型与地址、MMIO 范围、PCI 配置、中断控制器地址等）。

#### [`rust-toolchain.toml`](rust-toolchain.toml) —— 工具链配置

```toml
[toolchain]
profile = "minimal"
channel = "nightly-2025-12-12"
components = ["rust-src", "llvm-tools"]
targets = [
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "x86_64-unknown-none",
    "loongarch64-unknown-none",
]
```

- 使用 `nightly-2025-12-12` 版本，因为 ArceOS 依赖 nightly 特性（如 `no_std`、内联汇编等）。
- `rust-src`：编译 `no_std` 目标所需的标准库源码。
- `llvm-tools`：提供 `rust-objcopy` 等工具。
- 四个裸机目标三元组对应四种支持的架构。

#### [`scripts/test.sh`](scripts/test.sh) —— 验证脚本

测试脚本遍历四种架构，对每种架构执行以下检查：

1. **QEMU 可用性检查**：检测 `qemu-system-{arch}` 是否存在，不存在则跳过。
2. **文本内容检查**：运行 `cargo xtask run --arch={arch}`，验证串口输出包含 `Hello, Arceos!`。
3. **ANSI 颜色码检查**：使用正则表达式 `\x1b\[[0-9;]*[1-9][0-9;]*m` 检测输出中是否包含 ANSI SGR 颜色设置序列（排除纯重置序列 `\x1b[0m`）。

### 3.3 依赖链

ArceOS 采用分层架构，从应用层到硬件层的完整依赖链如下：

```
arceos-printcolor (应用层)
    └── axstd (标准库层，提供 println! 等宏)
        └── arceos_api (API 层，封装内核功能接口)
            ├── axhal (硬件抽象层，平台相关操作)
            ├── axruntime (运行时层，初始化与启动)
            ├── axlog (日志层，输出实现)
            ├── axconfig (配置层，读取 .axconfig.toml)
            ├── axsync (同步原语层)
            └── ... (其他组件)
```

输出流程：`println!` → `axstd` → `arceos_api` → `axlog` → `axhal` → UART 硬件

---

## 4. 实验内容

本练习要求在 ArceOS unikernel 中输出带有 ANSI 颜色转义序列的 "Hello, Arceos!" 文本。具体验证标准如下：

1. **文本存在性**：串口输出必须包含字符串 `Hello, Arceos!`。
2. **ANSI 颜色码**：输出必须包含 ANSI SGR 颜色设置序列，匹配正则表达式 `\x1b\[[0-9;]*[1-9][0-9;]*m`。

该正则表达式的含义：
- `\x1b`：ESC 字符（ASCII 27）
- `\[`：CSI（Control Sequence Introducer）
- `[0-9;]*`：零个或多个数字和分号（参数部分）
- `[1-9][0-9;]*m`：至少一个非零数字开头的参数后跟 `m`（SGR 命令）

此正则排除了纯重置序列 `\x1b[0m` 和 `\x1b[m`，确保检测到的是实际的颜色设置序列。

---

## 5. 实现方法

### 5.1 ANSI 转义序列原理

ANSI 转义序列是一种用于控制终端文本显示格式的标准机制。其基本格式为：

```
ESC [ <参数> m
```

其中：
- `ESC`（`\x1b`，ASCII 27）：转义字符，标志序列开始
- `[`：CSI（Control Sequence Introducer），控制序列引导符
- `<参数>`：由分号分隔的数字参数
- `m`：SGR（Select Graphic Rendition）命令字符

常用的 SGR 参数：

| 参数 | 效果 |
|------|------|
| `0` | 重置所有属性 |
| `1` | 粗体 |
| `30`–`37` | 前景色（黑、红、绿、黄、蓝、品红、青、白） |
| `40`–`47` | 背景色 |
| `32` | 绿色前景 |

### 5.2 实现思路

在原始代码中，[`println!`](src/main.rs:9) 输出的文本不包含颜色控制：

```rust
println!("[WithColor]: Hello, Arceos!");
```

为实现彩色输出，需要在文本前后添加 ANSI SGR 序列：

1. **设置颜色**：在文本前添加 `\x1b[32m`，将前景色设为绿色。
2. **重置颜色**：在文本后添加 `\x1b[0m`，恢复默认颜色，避免影响后续输出。

### 5.3 为什么选择绿色

选择 `\x1b[32m`（绿色前景）的原因：

1. **匹配测试正则**：`\x1b[32m` 中参数部分为 `32`，首数字为 `3`（非零），匹配正则 `\x1b\[[0-9;]*[1-9][0-9;]*m`。
2. **可读性好**：绿色在深色终端背景上具有良好的可读性。
3. **简洁性**：单参数 SGR 序列，实现简单直观。

---

## 6. 关键代码

修改位于 [`src/main.rs`](src/main.rs:9) 第 9 行：

**修改前：**

```rust
println!("[WithColor]: Hello, Arceos!");
```

**修改后：**

```rust
println!("\x1b[32m[WithColor]: Hello, Arceos!\x1b[0m");
```

**代码解析：**

| 部分 | 含义 |
|------|------|
| `\x1b[32m` | ANSI SGR 序列：设置前景色为绿色 |
| `[WithColor]: Hello, Arceos!` | 实际输出的文本内容 |
| `\x1b[0m` | ANSI SGR 序列：重置所有文本属性为默认值 |

完整文件内容：

```rust
#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
use axstd::println;

#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
fn main() {
    println!("\x1b[32m[WithColor]: Hello, Arceos!\x1b[0m");
}
```

**注意事项：**

- 在 Rust 字符串中，`\x1b` 表示 ASCII 值为 27 的 ESC 字符。
- 必须在文本末尾添加 `\x1b[0m` 重置序列，否则后续所有终端输出都将保持绿色。
- 由于 ArceOS 的 `println!` 宏最终通过 UART 串口输出，ANSI 转义序列会原样发送到串口，由终端模拟器解析并渲染颜色。

---

## 7. 测试验证

### 7.1 测试方法

运行 [`scripts/test.sh`](scripts/test.sh) 验证脚本：

```bash
bash scripts/test.sh
```

该脚本会依次对 riscv64、x86_64、aarch64、loongarch64 四种架构执行构建、运行和输出验证。

### 7.2 测试结果

| 架构 | QEMU 可用 | 文本检查 | 颜色码检查 | 结果 |
|------|-----------|----------|------------|------|
| riscv64 | ✓ | ✓ | ✓ | ✓ 彩色输出检测通过 |
| x86_64 | ✗ | — | — | ⊘ 跳过（QEMU 未安装） |
| aarch64 | ✗ | — | — | ⊘ 跳过（QEMU 未安装） |
| loongarch64 | ✗ | — | — | ⊘ 跳过（QEMU 未安装） |

**汇总：1 通过，0 失败，3 跳过 —— 所有已执行的测试均通过。**

### 7.3 结果分析

- **riscv64** 架构测试完全通过，验证了 ANSI 颜色转义序列在 ArceOS 串口输出中的正确性。
- x86_64、aarch64、loongarch64 三种架构因当前环境未安装对应的 QEMU 模拟器而跳过，并非功能缺陷。
- 由于 ArceOS 的输出机制与架构无关（均通过 `axhal` 的 UART 驱动输出），ANSI 转义序列作为纯文本数据，在所有架构上的行为一致，因此 riscv64 的通过可视为功能正确性的充分验证。

---

## 8. 实验总结

### 8.1 实验收获

通过本次实验，我获得了以下方面的深入理解：

1. **ArceOS Unikernel 架构**：了解了 ArceOS 的组件化设计思想——从 `axstd`（标准库）到 `axhal`（硬件抽象层）的分层架构，以及各组件通过 `axconfig` 进行配置解耦的机制。这种设计使得同一份应用代码可以在不同架构上运行，只需更换底层配置和平台包。

2. **裸机构建流程**：掌握了 `no_std`/`no_main` 环境下的完整构建流程，包括：链接器脚本的自动定位与配置、ELF 到原始二进制的转换（`rust-objcopy`）、以及各架构 QEMU 的启动参数差异。

3. **ANSI 转义序列机制**：深入理解了 ANSI SGR 控制序列的格式规范，以及终端如何解析和渲染这些序列。在裸机环境中，ANSI 转义序列作为普通字符通过 UART 传输，由接收端的终端模拟器负责解析和渲染，内核本身无需实现颜色渲染逻辑。

4. **多架构验证方法**：学习了使用 xtask 工具统一管理多架构构建，以及通过自动化脚本进行功能验证的最佳实践。

### 8.2 关键认识

- **输出即字节流**：在操作系统内核开发中，"彩色输出"并不意味着内核需要理解颜色——内核只需将包含 ANSI 转义序列的字节流通过串口发送出去，颜色渲染完全由终端模拟器负责。这种关注点分离的设计使得内核实现保持简洁。

- **配置驱动架构**：ArceOS 通过 `.axconfig.toml` 配置文件将平台参数（内存布局、设备地址等）从代码中分离，配合 `axhal` 的平台抽象，实现了"一次编写，多架构运行"的目标。

- **测试正则的设计**：测试脚本使用的正则 `\x1b\[[0-9;]*[1-9][0-9;]*m` 巧妙地排除了纯重置序列 `\x1b[0m`，确保检测到的是实际的颜色设置而非无意义的重置操作，体现了测试设计的严谨性。
