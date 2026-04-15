# 实验报告：ArceOS Bump 分配器实现

## 1. 实验概述

本实验是 [ArceOS](https://github.com/arceos-org/arceos) unikernel 的内存分配器练习项目。ArceOS 是一个基于组件化设计的 unikernel 框架，其内存分配子系统采用可插拔架构，支持多种分配算法。

**实验目标**：实现一个 bump 风格的内存分配器（`bump_allocator`），并将其集成到 ArceOS 的全局分配器框架中。

**测试程序**：在 [`src/main.rs`](src/main.rs) 中，测试程序分配 300 万个 `usize`（约 24 MB），对它们进行排序，并验证排序结果的正确性。这一测试同时考察了分配器的正确性与性能。

```rust
const N: usize = 3_000_000;
let mut v = Vec::with_capacity(N);
for i in 0..N {
    v.push(i);
}
v.sort();
for i in 0..N - 1 {
    assert!(v[i] <= v[i + 1]);
}
```

---

## 2. 代码框架

### 2.1 项目整体架构

项目采用分层架构，从应用层到分配器的调用链如下：

```
┌─────────────────────────────────────────────────────────────────┐
│                        应用层 (Application)                      │
│                     src/main.rs - 测试主程序                      │
│                   Vec::push / Vec::sort / assert                 │
└──────────────────────────┬──────────────────────────────────────┘
                           │ Rust 全局分配器接口 (GlobalAlloc)
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                   全局分配器 (GlobalAllocator)                    │
│            modules/axalloc/src/default_impl.rs                   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  level-1 模式:                                           │    │
│  │    balloc: SpinNoIrq<DefaultByteAllocator>               │    │
│  │    (无独立 palloc，字节和页分配都委托给 balloc)            │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                 │
│  DefaultByteAllocator = bump_allocator::EarlyAllocator<PAGE_SIZE>│
└──────────────────────────┬──────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Bump 分配器 (EarlyAllocator)                   │
│              modules/bump_allocator/src/lib.rs                   │
│                                                                 │
│   [ bytes-used | avail-area | pages-used ]                      │
│   |            | -->    <-- |            |                       │
│   start       b_pos        p_pos       end                      │
│                                                                 │
│   实现: BaseAllocator + ByteAllocator + PageAllocator            │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 关键机制：Cargo Patch

根 [`Cargo.toml`](Cargo.toml) 通过 `[patch.crates-io]` 将 `axalloc` crate 重定向到本地 `modules/axalloc` 目录：

```toml
[patch.crates-io]
axalloc = { path = "modules/axalloc" }
```

这一机制使得我们可以替换 ArceOS 标准的 `axalloc` 实现，使用自定义的 bump 分配器，而无需修改上层框架代码。

### 2.3 特性配置

[`modules/axalloc/Cargo.toml`](modules/axalloc/Cargo.toml) 中默认启用 `bump_allocator` + `level-1` 特性：

```toml
[features]
default = [
    "bump_allocator",
    "level-1",
    "axallocator/page-alloc-256m",
]
bump_allocator = [
    "dep:axallocator",
    "dep:bump_allocator",
]
level-1 = []
```

在 `level-1` 模式下，[`DefaultByteAllocator`](modules/axalloc/src/default_impl.rs:36) 被定义为 `bump_allocator::EarlyAllocator<PAGE_SIZE>`，且 [`GlobalAllocator`](modules/axalloc/src/default_impl.rs:46) 不包含独立的 `palloc` 字段——所有分配（字节和页）都委托给 `balloc`（即 `EarlyAllocator`）。

### 2.4 各文件功能说明

| 文件路径 | 功能说明 |
|---------|---------|
| [`Cargo.toml`](Cargo.toml) | 项目配置，通过 `[patch.crates-io]` 将 `axalloc` 重定向到本地模块 |
| [`src/main.rs`](src/main.rs) | 测试主程序，分配 300 万个 `usize` 并排序验证 |
| [`modules/axalloc/`](modules/axalloc/) | 全局分配器框架目录 |
| [`modules/axalloc/src/lib.rs`](modules/axalloc/src/lib.rs) | 模块入口，定义 [`UsageKind`](modules/axalloc/src/lib.rs:29)/[`Usages`](modules/axalloc/src/lib.rs:46) 类型，根据特性选择实现 |
| [`modules/axalloc/src/default_impl.rs`](modules/axalloc/src/default_impl.rs) | [`GlobalAllocator`](modules/axalloc/src/default_impl.rs:46) 实现，level-1 模式下字节和页分配都走 `balloc` |
| [`modules/axalloc/src/page.rs`](modules/axalloc/src/page.rs) | [`GlobalPage`](modules/axalloc/src/page.rs:10) RAII 包装，自动释放页分配 |
| [`modules/bump_allocator/`](modules/bump_allocator/) | 需要实现的 bump 分配器目录 |
| [`modules/bump_allocator/src/lib.rs`](modules/bump_allocator/src/lib.rs) | [`EarlyAllocator`](modules/bump_allocator/src/lib.rs:21) 实现，本实验核心代码 |
| [`configs/`](configs/) | 各架构 QEMU 运行配置（riscv64、aarch64、x86_64、loongarch64） |

---

## 3. Bump 分配算法原理

### 3.1 基本原理

Bump 分配器（也称指针推进分配器）是最简单的内存分配算法之一。其核心思想是维护一个指针，每次分配时将指针向前推移相应大小，返回推移前的位置作为分配结果。分配操作的时间复杂度为 O(1)，无需遍历空闲链表或搜索位图。

### 3.2 双端 Bump 分配器设计

本实验采用**双端 Bump 分配器**设计，将同一块内存区域从两端分别用于字节分配和页分配：

- **字节分配**：从低地址向高地址增长，由 `b_pos` 指针追踪
- **页分配**：从高地址向低地址增长，由 `p_pos` 指针追踪
- 两者共享中间的空闲区域

内存布局如下：

```
低地址                                                  高地址
┌──────────────┬──────────────────────┬──────────────┐
│  bytes-used  │     avail-area       │  pages-used  │
│              │                      │              │
│  已分配字节   │     可用空间          │  已分配页     │
│              │                      │              │
└──────────────┴──────────────────────┴──────────────┘
^              ^                      ^              ^
start         b_pos                  p_pos       start+size

b_pos 向高地址推移 (字节分配) ──────►
                ◄────── p_pos 向低地址推移 (页分配)
```

### 3.3 对齐处理

- **字节分配**：将 `b_pos` 向上对齐到 `layout.align()`，确保返回的内存地址满足对齐要求
  ```
  b_pos = (b_pos + align - 1) & !(align - 1)
  ```
- **页分配**：将 `p_pos` 向下对齐到 `align_pow2`，确保页分配的起始地址满足对齐要求
  ```
  p_pos = p_pos & !(align_pow2 - 1)
  ```

### 3.4 释放策略

| 分配类型 | 释放策略 | 说明 |
|---------|---------|------|
| 字节分配 | 引用计数批量释放 | 维护 `count` 计数器，每次分配递增，每次释放递减；当 `count` 归零时，重置 `b_pos = start`，一次性回收所有字节分配空间 |
| 页分配 | 永不释放 | `dealloc_pages()` 为空操作，已分配的页空间无法回收 |

### 3.5 优缺点分析

| 方面 | 优点 | 缺点 |
|------|------|------|
| **性能** | 分配 O(1)，无搜索开销 | 释放受限，页分配永不回收 |
| **实现** | 代码简洁，易于理解和实现 | 功能有限，不适合通用场景 |
| **内存利用** | 双端设计最大化利用空闲区域 | 字节释放只能批量，无法单独回收 |
| **碎片** | 无外部碎片 | 可能产生内部碎片（对齐开销） |
| **适用场景** | 早期启动阶段、简单分配需求 | 不适合需要频繁释放的长期运行场景 |

---

## 4. 实现方法

本节详细说明 [`EarlyAllocator<const PAGE_SIZE: usize>`](modules/bump_allocator/src/lib.rs:21) 的实现。

### 4.1 内部状态

```rust
pub struct EarlyAllocator<const PAGE_SIZE: usize> {
    start: usize,   // 内存区域基址
    size: usize,    // 区域总大小
    b_pos: usize,   // 字节分配指针（从低地址向高地址推移）
    p_pos: usize,   // 页分配指针（从高地址向低地址推移）
    count: usize,   // 活跃字节分配计数（用于批量释放）
}
```

各字段含义：

| 字段 | 类型 | 含义 |
|------|------|------|
| `start` | `usize` | 内存区域的起始地址，初始化后不变 |
| `size` | `usize` | 内存区域的总大小（字节），`add_memory` 时可扩展 |
| `b_pos` | `usize` | 字节分配的当前指针，分配时向高地址推移 |
| `p_pos` | `usize` | 页分配的当前指针，分配时向低地址推移 |
| `count` | `usize` | 当前活跃的字节分配数量，归零时重置 `b_pos` |

### 4.2 BaseAllocator 实现

[`BaseAllocator`](modules/bump_allocator/src/lib.rs:41) trait 提供初始化和内存区域管理功能：

#### `init()`

```rust
fn init(&mut self, start: usize, size: usize) {
    self.start = start;
    self.size = size;
    self.b_pos = start;          // 字节分配从区域起始位置开始
    self.p_pos = start + size;   // 页分配从区域末尾开始
    self.count = 0;
}
```

初始化时，`b_pos` 设为区域起始地址，`p_pos` 设为区域结束地址，两者之间的空间即为可用区域。

#### `add_memory()`

```rust
fn add_memory(&mut self, _start: usize, size: usize) -> AllocResult {
    self.p_pos += size;   // 扩展页分配指针
    self.size += size;    // 更新总大小
    Ok(())
}
```

扩展内存区域时，将新空间追加到高地址端，因此 `p_pos` 和 `size` 相应增加。

### 4.3 ByteAllocator 实现

[`ByteAllocator`](modules/bump_allocator/src/lib.rs:57) trait 提供字节级别的分配和释放功能：

#### `alloc()`

```rust
fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
    // 1. 将 b_pos 向上对齐到 layout.align()
    self.b_pos = (self.b_pos + layout.align() - 1) & !(layout.align() - 1);
    // 2. 检查剩余空间是否足够
    if self.b_pos + layout.size() > self.p_pos {
        return Err(AllocError::NoMemory);
    }
    // 3. 记录分配位置，推进 b_pos
    let ptr = self.b_pos;
    self.b_pos += layout.size();
    // 4. 递增分配计数
    self.count += 1;
    Ok(NonNull::new(ptr as *mut u8).unwrap())
}
```

分配流程：
1. **对齐**：将 `b_pos` 向上对齐到请求的对齐边界
2. **空间检查**：确保对齐后的 `b_pos` 加上请求大小不超过 `p_pos`
3. **推进指针**：记录当前 `b_pos` 作为分配结果，然后推进 `b_pos`
4. **计数递增**：增加活跃分配计数

#### `dealloc()`

```rust
fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
    self.count -= 1;
    if self.count == 0 {
        self.b_pos = self.start;   // 所有字节分配已释放，重置指针
    }
}
```

释放逻辑：
- 递减活跃分配计数
- 当计数归零时，重置 `b_pos` 到 `start`，一次性回收所有字节分配空间
- 注意：不支持单独释放某个分配，仅支持批量释放

#### 统计方法

```rust
fn total_bytes(&self) -> usize {
    self.size                          // 总大小
}
fn used_bytes(&self) -> usize {
    self.b_pos - self.start            // 已用字节 = b_pos - start
}
fn available_bytes(&self) -> usize {
    self.p_pos - self.b_pos            // 可用字节 = p_pos - b_pos
}
```

### 4.4 PageAllocator 实现

[`PageAllocator`](modules/bump_allocator/src/lib.rs:91) trait 提供页级别的分配功能：

#### `alloc_pages()`

```rust
fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
    let total = num_pages * PAGE_SIZE;
    // 1. 将 p_pos 向下对齐到 align_pow2
    self.p_pos = self.p_pos & !(align_pow2 - 1);
    // 2. 计算新的 p_pos
    let new_p_pos = self.p_pos - total;
    // 3. 检查是否与 b_pos 冲突
    if new_p_pos < self.b_pos {
        return Err(AllocError::NoMemory);
    }
    self.p_pos = new_p_pos;
    Ok(self.p_pos)   // 返回分配的起始地址
}
```

分配流程：
1. **对齐**：将 `p_pos` 向下对齐到请求的对齐边界
2. **计算新位置**：从对齐后的 `p_pos` 减去所需总大小
3. **冲突检查**：确保新的 `p_pos` 不低于 `b_pos`，避免字节区域和页区域重叠
4. **返回地址**：返回新的 `p_pos` 作为分配的起始地址

#### `dealloc_pages()`

```rust
fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
    // 空操作：bump 分配器永不释放页
}
```

页分配一旦完成就不可回收，这是 bump 分配器的设计限制。

#### `alloc_pages_at()`

```rust
fn alloc_pages_at(
    &mut self,
    _base: usize,
    _num_pages: usize,
    _align_pow2: usize,
) -> AllocResult<usize> {
    Err(AllocError::NoMemory)   // 不支持指定地址分配
}
```

Bump 分配器不支持在指定地址分配页，直接返回错误。

#### 统计方法

```rust
fn total_pages(&self) -> usize {
    self.size / PAGE_SIZE                              // 总页数
}
fn used_pages(&self) -> usize {
    (self.start + self.size - self.p_pos) / PAGE_SIZE  // 已用页数
}
fn available_pages(&self) -> usize {
    (self.p_pos - self.start) / PAGE_SIZE              // 可用页数
}
```

---

## 5. 关键代码

以下是 [`modules/bump_allocator/src/lib.rs`](modules/bump_allocator/src/lib.rs) 的完整实现代码，逐段添加注释说明：

```rust
#![no_std]  // 不使用标准库，适配内核环境

use axallocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
use core::alloc::Layout;
use core::ptr::NonNull;

/// 早期内存分配器
/// 在正式的字节分配器和页分配器可用之前使用！
/// 这是一个双端内存范围分配器：
/// - 字节分配向前（低地址→高地址）推进
/// - 页分配向后（高地址→低地址）推进
///
/// 内存布局：
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start       b_pos        p_pos       end
///
/// 对于字节区域，'count' 记录分配数量。
/// 当 count 降为零时，释放所有已使用的字节区域。
/// 对于页区域，永远不会被释放！
///
/// PAGE_SIZE 为编译期常量泛型参数，表示页的大小
pub struct EarlyAllocator<const PAGE_SIZE: usize> {
    start: usize,   // 内存区域基址
    size: usize,    // 区域总大小（字节）
    b_pos: usize,   // 字节分配指针（向高地址推移）
    p_pos: usize,   // 页分配指针（向低地址推移）
    count: usize,   // 活跃字节分配计数
}

// ─── 构造函数 ───────────────────────────────────────────────

impl<const PAGE_SIZE: usize> EarlyAllocator<PAGE_SIZE> {
    /// 创建一个空的分配器实例
    /// 所有字段初始化为零，需调用 init() 后才能使用
    pub const fn new() -> Self {
        Self {
            start: 0,
            size: 0,
            b_pos: 0,
            p_pos: 0,
            count: 0,
        }
    }
}

// ─── BaseAllocator 实现 ─────────────────────────────────────

impl<const PAGE_SIZE: usize> BaseAllocator for EarlyAllocator<PAGE_SIZE> {
    /// 初始化分配器，设定管理的内存区域
    /// start: 区域起始地址
    /// size: 区域大小（字节）
    fn init(&mut self, start: usize, size: usize) {
        self.start = start;
        self.size = size;
        self.b_pos = start;          // 字节分配从最低地址开始
        self.p_pos = start + size;   // 页分配从最高地址开始
        self.count = 0;
    }

    /// 向分配器添加额外的内存区域
    /// 新空间追加到高地址端，因此扩展 p_pos
    fn add_memory(&mut self, _start: usize, size: usize) -> AllocResult {
        self.p_pos += size;
        self.size += size;
        Ok(())
    }
}

// ─── ByteAllocator 实现 ─────────────────────────────────────

impl<const PAGE_SIZE: usize> ByteAllocator for EarlyAllocator<PAGE_SIZE> {
    /// 分配指定布局的字节空间
    /// layout 包含大小和对齐要求
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        // 步骤1：将 b_pos 向上对齐到 layout.align()
        // 对齐公式：(pos + align - 1) & !(align - 1)
        self.b_pos = (self.b_pos + layout.align() - 1) & !(layout.align() - 1);

        // 步骤2：检查对齐后的 b_pos + 请求大小是否超过 p_pos
        // 如果超过，说明可用空间不足
        if self.b_pos + layout.size() > self.p_pos {
            return Err(AllocError::NoMemory);
        }

        // 步骤3：记录当前 b_pos 作为分配结果的起始地址
        let ptr = self.b_pos;
        // 推进 b_pos，标记这部分空间已分配
        self.b_pos += layout.size();

        // 步骤4：递增活跃分配计数
        self.count += 1;

        // 将地址转换为 NonNull 指针返回
        Ok(NonNull::new(ptr as *mut u8).unwrap())
    }

    /// 释放之前分配的字节空间
    /// 注意：bump 分配器不支持单独释放，采用引用计数批量释放策略
    fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
        // 递减活跃分配计数
        self.count -= 1;
        // 当所有字节分配都已释放（count 归零），重置 b_pos
        // 这样整个字节区域就可以被重新使用
        if self.count == 0 {
            self.b_pos = self.start;
        }
    }

    /// 返回管理的总字节数
    fn total_bytes(&self) -> usize {
        self.size
    }

    /// 返回已使用的字节数（b_pos 与 start 之间的距离）
    fn used_bytes(&self) -> usize {
        self.b_pos - self.start
    }

    /// 返回可用的字节数（p_pos 与 b_pos 之间的距离）
    /// 这是字节分配和页分配共享的空闲区域
    fn available_bytes(&self) -> usize {
        self.p_pos - self.b_pos
    }
}

// ─── PageAllocator 实现 ─────────────────────────────────────

impl<const PAGE_SIZE: usize> PageAllocator for EarlyAllocator<PAGE_SIZE> {
    /// 页大小常量，由泛型参数指定
    const PAGE_SIZE: usize = PAGE_SIZE;

    /// 分配指定数量的连续页
    /// num_pages: 页数
    /// align_pow2: 对齐要求（2的幂）
    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        // 计算所需的总字节数
        let total = num_pages * PAGE_SIZE;

        // 步骤1：将 p_pos 向下对齐到 align_pow2
        // 对齐公式：pos & !(align - 1)
        self.p_pos = self.p_pos & !(align_pow2 - 1);

        // 步骤2：计算新的 p_pos（向低地址推移 total 字节）
        let new_p_pos = self.p_pos - total;

        // 步骤3：检查新的 p_pos 是否低于 b_pos
        // 如果低于，说明字节区域和页区域将发生重叠，空间不足
        if new_p_pos < self.b_pos {
            return Err(AllocError::NoMemory);
        }

        // 步骤4：更新 p_pos 并返回分配的起始地址
        self.p_pos = new_p_pos;
        Ok(self.p_pos)
    }

    /// 释放页 —— 空操作
    /// bump 分配器的设计决定：页分配后永不回收
    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // No-op: bump allocator never frees pages
    }

    /// 在指定地址分配页 —— 不支持
    /// bump 分配器只能从 p_pos 处分配，无法指定基地址
    fn alloc_pages_at(
        &mut self,
        _base: usize,
        _num_pages: usize,
        _align_pow2: usize,
    ) -> AllocResult<usize> {
        Err(AllocError::NoMemory)
    }

    /// 返回总页数
    fn total_pages(&self) -> usize {
        self.size / PAGE_SIZE
    }

    /// 返回已使用的页数
    // 已用页区域 = (start + size) - p_pos，即高地址端被页分配占用的部分
    fn used_pages(&self) -> usize {
        (self.start + self.size - self.p_pos) / PAGE_SIZE
    }

    /// 返回可用页数
    // 可用区域 = p_pos - start，但其中部分可能被字节分配占用
    fn available_pages(&self) -> usize {
        (self.p_pos - self.start) / PAGE_SIZE
    }
}
```

---

## 6. 测试与验证

### 6.1 测试方法

使用 ArceOS 的 xtask 工具在 QEMU 上运行测试：

```bash
cargo xtask run --arch=riscv64
```

也可指定其他架构：

```bash
cargo xtask run --arch=aarch64
cargo xtask run --arch=x86_64
```

### 6.2 测试程序分析

[`src/main.rs`](src/main.rs) 中的测试程序执行以下步骤：

1. **分配**：创建一个容量为 300 万的 `Vec<usize>`，逐个压入元素
   - 总内存需求：3,000,000 × 8 bytes = 24,000,000 bytes ≈ 24 MB
2. **排序**：对 Vec 进行排序，涉及大量元素的移动和比较
3. **验证**：遍历排序结果，确认每个元素不大于其后继元素

该测试覆盖了分配器的核心功能：
- 大量连续分配（`Vec::push` 触发堆分配）
- 分配器对齐处理（`Vec` 内部分配需满足对齐要求）
- 排序过程中的临时内存需求

### 6.3 预期输出

```
Running bump tests...
Bump tests run OK!
```

### 6.4 实际验证结果

在 riscv64 QEMU 上运行成功，输出符合预期，确认 bump 分配器实现正确。

---

## 7. 实验总结

### 7.1 实验收获

通过本实验，深入理解了以下内容：

1. **Bump 分配算法原理**：掌握了最简单的内存分配算法的核心思想——通过指针推进实现 O(1) 分配，以及双端设计如何让字节分配和页分配共享同一块内存区域。

2. **ArceOS 全局分配器框架**：理解了 ArceOS 的分层分配器架构，包括：
   - [`GlobalAllocator`](modules/axalloc/src/default_impl.rs:46) 如何实现 `GlobalAlloc` trait 并注册为 Rust 全局分配器
   - [`UsageKind`](modules/axalloc/src/lib.rs:29)/[`Usages`](modules/axalloc/src/lib.rs:46) 内存使用追踪机制
   - [`GlobalPage`](modules/axalloc/src/page.rs:10) RAII 包装如何自动管理页的生命周期

3. **Level-1 模式下字节/页分配统一处理**：在 `level-1` 模式下，[`GlobalAllocator`](modules/axalloc/src/default_impl.rs:46) 不包含独立的页分配器，页分配请求通过构造 `Layout` 委托给字节分配器处理。这种设计简化了分配器架构，适合早期启动阶段或简单场景。

### 7.2 双端设计的优势

双端 Bump 分配器的设计使得字节分配和页分配可以动态共享中间的空闲区域，无需预先划分固定比例。这带来了以下优势：

- **最大化内存利用率**：空闲区域完全共享，避免了因固定划分导致的浪费
- **自适应分配模式**：如果应用主要进行字节分配，`b_pos` 可以向高地址推进更远；如果主要进行页分配，`p_pos` 可以向低地址推进更远
- **实现简洁**：仅需维护两个指针和一个计数器，代码量极少

### 7.3 局限性与改进方向

当前实现的局限性主要包括：

- 字节分配仅支持批量释放，无法单独回收特定分配
- 页分配完全不支持释放
- 不适合需要长期运行和频繁分配/释放的场景

可能的改进方向：
- 实现更复杂的分配算法（如 SLAB、TLSF、Buddy）作为后续分配器
- 在 bump 分配器基础上构建二级分配器，bump 分配器作为底层内存来源
- 添加分配器统计和调试信息输出
