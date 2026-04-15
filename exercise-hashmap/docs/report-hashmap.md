# 实验报告：exercise-hashmap — 在 axstd 中实现 HashMap 支持

## 一、实验目的

在 ArceOS unikernel 的标准库 `axstd` 中实现 `collections::HashMap` 支持，使得 `no_std` 裸机环境下可以使用 `HashMap` 数据结构。

## 二、代码框架

本项目基于 ArceOS unikernel 架构，是一个 `no_std` 裸机环境下的 Rust 程序。核心框架组成：

| 组件 | 路径 | 说明 |
|---|---|---|
| 应用程序入口 | `src/main.rs` | 使用 `axstd` 作为 `std` 的替代，通过 `extern crate axstd as std` 实现别名 |
| ArceOS 标准库 | `axstd/` | 本地补丁版本，替代 Rust `std`，提供基本 I/O、集合等支持 |
| 构建运行工具 | `xtask/src/main.rs` | 封装 `cargo build` + `rust-objcopy` + QEMU 启动 |
| 构建脚本 | `build.rs` | 自动发现链接脚本，传递链接器参数 |
| 平台配置 | `configs/` | 各架构平台配置（riscv64/aarch64/x86_64/loongarch64） |
| 测试脚本 | `scripts/test.sh` | 多架构自动化验证脚本 |

构建流程：`cargo xtask run --arch riscv64` → 复制配置到 `.axconfig.toml` → 交叉编译 → objcopy 转换 → QEMU 启动运行

## 三、实验内容

`src/main.rs` 中的测试函数 `test_hashmap()` 执行以下操作：
1. 创建一个 `HashMap`，插入 50,000 个键值对（`"key_0"` → `0`, `"key_1"` → `1`, ...）
2. 遍历 HashMap，验证每个键值对的正确性
3. 输出 `test_hashmap() OK!`

**核心问题**：`axstd` 原本只通过 `alloc::collections` 重导出 `BTreeMap`、`BTreeSet`、`VecDeque` 等集合类型，而 `HashMap`/`HashSet` 不在 `alloc` crate 中（它们需要哈希器提供随机种子），因此 `std::collections::HashMap` 无法解析，编译失败。

## 四、实现方法

核心思路：将 `axstd` 克隆到本地，添加 `hashbrown` 依赖（其 `default-hasher` 特性引入 `foldhash`，可在 `no_std` 下工作），创建自定义 `collections` 模块重导出 `HashMap`/`HashSet`。

### 4.1 将 axstd 克隆到本地

从 cargo registry 复制 `axstd@0.3.0-preview.1` 源码到项目目录 `./axstd/`，并在项目 `Cargo.toml` 中添加 patch：

```toml
[patch.crates-io]
axstd = { path = "./axstd" }
```

### 4.2 创建 `axstd/src/collections/mod.rs`

新建 `collections` 模块，重导出 `alloc::collections` 的所有类型，并额外导出 `hashbrown` 的 `HashMap` 和 `HashSet`：

```rust
//! Collection types.

#[cfg(feature = "alloc")]
pub use alloc::collections::*;

#[cfg(feature = "alloc")]
pub use hashbrown::{HashMap, HashSet};
```

### 4.3 修改 `axstd/src/lib.rs`

将原来的直接重导出：
```rust
pub use alloc::{boxed, collections, format, string, vec};
```
改为：
```rust
pub use alloc::{boxed, format, string, vec};

pub mod collections;
```

移除 `collections` 的直接重导出，改为声明自定义模块，使得 `std::collections::HashMap` 可以正确解析。

### 4.4 在 `axstd/Cargo.toml` 添加 hashbrown 依赖

```toml
[dependencies.hashbrown]
version = "0.16"
default-features = false
features = ["default-hasher", "equivalent", "raw-entry"]
```

**关键点**：`default-hasher` 特性引入 `foldhash`，它通过栈指针地址和原子计数器生成哈希种子，无需 OS 级随机数源，完全兼容 `no_std` 环境。`foldhash` 的 `gen_per_hasher_seed()` 使用栈指针地址 + 全局原子计数器实现非确定性，`GlobalSeed::new()` 使用地址空间布局随机化（栈/函数/静态指针）+ 基于原子的惰性初始化。

## 五、实验结果

`cargo xtask run --arch riscv64` 编译运行成功，QEMU 输出：

```
Running memory tests...
test_hashmap() OK!
Memory tests run OK!
```

50,000 个键值对的插入与验证全部通过，`HashMap` 在 ArceOS unikernel 环境下正常工作。

## 六、实验总结

本实验的核心挑战在于 `no_std` 环境下 `HashMap` 需要哈希器提供随机种子，而裸机环境没有操作系统提供的随机数接口。通过引入 `hashbrown` + `foldhash`，利用栈指针地址和原子计数器作为熵源，成功在无 OS 随机数支持的情况下实现了功能完整的 `HashMap`。整个实现只需修改 `axstd` 的三个文件（`Cargo.toml`、`lib.rs`、新增 `collections/mod.rs`）和项目 `Cargo.toml` 的 patch 配置，改动量小且不影响现有功能。
