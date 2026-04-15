# ArceOS ramfs rename 支持 — 实验报告

---

## 1. 实验概述

| 项目 | 内容 |
|------|------|
| **实验名称** | ArceOS ramfs rename 支持 |
| **实验目标** | 为 ArceOS 的内存文件系统 (ramfs) 添加 `rename` 操作支持 |
| **成功标准** | 程序运行输出 `[Ramfs-Rename]: ok!` |

ArceOS 是一个模块化的操作系统内核框架，其文件系统采用分层设计。在原始实现中，`VfsNodeOps` trait 的 [`rename()`](axfs_vfs/src/lib.rs:159) 方法具有默认实现，直接返回 `Err(Unsupported)`，而 `DirNode` 和 `RootDirectory` 均未覆写该方法。因此，调用 `std::fs::rename` 总是失败。本实验的目标是在 ramfs 及 VFS 分发层中实现 `rename`，使同目录下的文件重命名操作能够正确执行。

---

## 2. 代码框架分析

### 2.1 整体架构

ArceOS 的文件系统采用分层架构，从上到下依次为：

```
┌─────────────────────────────────────────────┐
│  Application (src/main.rs)                  │  用户应用层
│  调用 std::fs::rename()                     │
├─────────────────────────────────────────────┤
│  axstd                                      │  标准 API 层
│  将 std::fs::rename 映射到 axfs 接口        │
├─────────────────────────────────────────────┤
│  axfs (VFS 层)                              │  虚拟文件系统层
│  RootDirectory 作为组合节点，分发操作        │
├─────────────────────────────────────────────┤
│  axfs_vfs                                   │  VFS trait 定义层
│  定义 VfsNodeOps trait 及默认实现           │
├─────────────────────────────────────────────┤
│  axfs_ramfs                                 │  内存文件系统实现层
│  DirNode (目录) / FileNode (文件)           │
└─────────────────────────────────────────────┘
```

### 2.2 调用链

当应用程序调用 `std::fs::rename("/tmp/f1", "/tmp/f2")` 时，调用链如下：

1. **应用层** — [`src/main.rs`](src/main.rs) 中调用 `fs::rename(src, dst)`
2. **axstd 层** — `std::fs::rename` 映射到 `axfs::api::rename()`
3. **axfs VFS 层** — [`RootDirectory::rename()`](axfs/src/root.rs:184) 接收调用，根据路径分发到对应挂载的文件系统
4. **axfs_ramfs 层** — [`DirNode::rename()`](axfs_ramfs/src/dir.rs:168) 在目录的 `children` 中执行实际的键值替换

### 2.3 关键数据结构

#### `VfsNodeOps` trait（[`axfs_vfs/src/lib.rs`](axfs_vfs/src/lib.rs:87)）

定义了文件系统节点的操作接口，包括 `open`、`create`、`remove`、`lookup`、`read_dir` 等。其中 [`rename()`](axfs_vfs/src/lib.rs:159) 方法的默认实现为：

```rust
fn rename(&self, _src_path: &str, _dst_path: &str) -> VfsResult {
    ax_err!(Unsupported)
}
```

所有未显式实现 `rename` 的节点类型均会返回 `Unsupported` 错误。

#### `DirNode`（[`axfs_ramfs/src/dir.rs`](axfs_ramfs/src/dir.rs:14)）

内存文件系统中的目录节点，核心数据结构为：

```rust
pub struct DirNode {
    this: Weak<DirNode>,
    parent: RwLock<Weak<dyn VfsNodeOps>>,
    children: RwLock<BTreeMap<String, VfsNodeRef>>,
}
```

- `this`：自身的弱引用，用于创建子目录时传递父引用
- `parent`：父目录的弱引用，受读写锁保护
- `children`：子节点映射表，键为文件/目录名，值为 `VfsNodeRef`（`Arc<dyn VfsNodeOps>`），受读写锁保护

#### `RootDirectory`（[`axfs/src/root.rs`](axfs/src/root.rs:46)）

VFS 层的根目录组合节点，负责将操作分发到正确的挂载文件系统：

```rust
pub struct RootDirectory {
    main_fs: Arc<dyn VfsOps>,
    mounts: Vec<MountPoint>,
}
```

- `main_fs`：主文件系统
- `mounts`：挂载点列表，每个挂载点包含路径和对应的文件系统实例

`RootDirectory` 采用**组合模式 (Composite Pattern)**，对 `create`、`remove`、`lookup`、`rename` 等操作，先通过 [`normalize_path()`](axfs/src/root.rs:108) 规范化路径，再通过 [`find_best_mount()`](axfs/src/root.rs:119) 查找最匹配的挂载点，最后将操作委托给对应文件系统的根目录。

---

## 3. 实验内容

### 3.1 问题分析

crates.io 上发布的 `axfs`（v0.3.0-preview.1）和 `axfs_ramfs`（v0.1.2）均未实现 `rename` 操作：

- [`VfsNodeOps::rename()`](axfs_vfs/src/lib.rs:159) 的默认实现返回 `Err(Unsupported)`
- [`DirNode`](axfs_ramfs/src/dir.rs:14) 未覆写 `rename` 方法
- [`RootDirectory`](axfs/src/root.rs:46) 未覆写 `rename` 方法

因此，任何通过 `std::fs::rename` 发起的重命名请求都会失败。

### 3.2 测试程序

测试程序位于 [`src/main.rs`](src/main.rs)，其执行流程如下：

1. **创建目录** `/tmp`
2. **创建文件** `/tmp/f1` 并写入内容 `"hello"`
3. **读取并打印** `/tmp/f1` 的内容
4. **重命名** `/tmp/f1` 为 `/tmp/f2`
5. **读取并打印** `/tmp/f2` 的内容

```rust
fn process() -> io::Result<()> {
    create_dir("/tmp")?;
    create_file("/tmp/f1", "hello")?;
    print_file("/tmp/f1")?;
    rename_file("/tmp/f1", "/tmp/f2")?;  // 核心测试点
    print_file("/tmp/f2")
}
```

若 `rename` 未实现，程序将在第 4 步 panic；若实现正确，程序最终输出 `[Ramfs-Rename]: ok!`。

---

## 4. 实现方法

本实验对三个位置进行了修改，下面逐一详述。

### 4.1 Cargo.toml — 添加 `[patch.crates-io]`

在 [`Cargo.toml`](Cargo.toml) 末尾添加：

```toml
[patch.crates-io]
axfs = { path = "./axfs" }
axfs_ramfs = { path = "./axfs_ramfs" }
```

此配置覆盖 crates.io 上的发布版本，使项目使用本地修改后的 `axfs` 和 `axfs_ramfs` 源码。这是在不修改 `axstd` 依赖版本号的前提下，注入自定义实现的标准做法。

### 4.2 `axfs_ramfs/src/dir.rs` — DirNode 实现 rename

在 [`impl VfsNodeOps for DirNode`](axfs_ramfs/src/dir.rs:72) 中添加 [`rename()`](axfs_ramfs/src/dir.rs:168) 方法：

```rust
fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
    let (src_name, src_rest) = split_path(src_path);
    let (dst_name, dst_rest) = split_path(dst_path);

    if let Some(src_rest) = src_rest {
        // 源路径更深，递归进入子目录
        let child = self.children.read().get(src_name).cloned().ok_or(VfsError::NotFound)?;
        // 若目标路径首组件与源路径相同，剥离后递归；否则为跨目录重命名，不支持
        let dst = if src_name == dst_name {
            dst_rest.ok_or(VfsError::Unsupported)?
        } else {
            return Err(VfsError::Unsupported);
        };
        return child.rename(src_rest, dst);
    }

    // 叶子层：src_name 是当前目录中需要重命名的条目
    if src_name.is_empty() || src_name == "." || src_name == ".." {
        return Err(VfsError::InvalidInput);
    }

    if dst_rest.is_some() {
        // 跨目录重命名不支持
        return Err(VfsError::Unsupported);
    }

    if dst_name.is_empty() || dst_name == "." || dst_name == ".." {
        return Err(VfsError::InvalidInput);
    }

    let mut children = self.children.write();
    let node = children.remove(src_name).ok_or(VfsError::NotFound)?;
    children.insert(dst_name.into(), node);
    Ok(())
}
```

**实现逻辑详解：**

1. **路径解析**：使用 [`split_path()`](axfs_ramfs/src/dir.rs:209) 将路径拆分为首组件和剩余部分，与 `create`/`remove` 方法保持一致的模式

2. **递归处理**：当 `src_rest` 为 `Some` 时，说明源路径指向更深层级，需要递归进入子目录：
   - 从 `self.children` 中查找 `src_name` 对应的子节点
   - **关键**：检查目标路径的首组件 `dst_name` 是否与 `src_name` 相同——若相同，说明是同目录内的重命名（如 `/tmp/f1` → `/tmp/f2`，递归进入 `tmp` 时 `src_name == dst_name == "tmp"`），需剥离目标路径的首组件后递归；若不同，则为跨目录重命名，返回 `Unsupported`

3. **叶子层处理**：当 `src_rest` 为 `None` 时，`src_name` 即为当前目录中需要重命名的条目：
   - 校验源名和目标名不为空、`"."` 或 `".."`
   - 校验目标路径无剩余组件（防止跨目录）
   - 获取写锁，从 `children` 中移除旧条目，以新名称重新插入

### 4.3 `axfs/src/root.rs` — RootDirectory 实现 rename 分发

在 [`impl VfsNodeOps for RootDirectory`](axfs/src/root.rs:142) 中添加 [`rename()`](axfs/src/root.rs:184) 方法：

```rust
fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
    let src_path = self.normalize_path(src_path);
    let dst_path = self.normalize_path(dst_path);
    if let Some((mount_fs, rest_src)) = self.find_best_mount(&src_path) {
        if let Some((_, rest_dst)) = self.find_best_mount(&dst_path) {
            return mount_fs.root_dir().rename(rest_src, rest_dst);
        }
    }
    self.main_fs.root_dir().rename(&src_path, &dst_path)
}
```

**实现逻辑详解：**

此实现遵循与 [`create()`](axfs/src/root.rs:158) 和 [`remove()`](axfs/src/root.rs:171) 相同的分发模式：

1. **路径规范化**：调用 [`normalize_path()`](axfs/src/root.rs:108) 去除路径前导 `/` 和 `./` 前缀
2. **查找挂载点**：对源路径和目标路径分别调用 [`find_best_mount()`](axfs/src/root.rs:119)，查找最匹配的挂载文件系统
3. **委托执行**：若两个路径均匹配到挂载点，将操作委托给该文件系统的根目录执行；否则回退到主文件系统

---

## 5. 关键问题与解决

### 5.1 问题描述

在初始实现中，[`DirNode::rename()`](axfs_ramfs/src/dir.rs:168) 的递归分支直接将原始目标路径传递给子目录的 `rename` 调用，未做路径剥离处理。

以重命名 `/tmp/f1` 为 `/tmp/f2` 为例，调用过程如下：

1. `RootDirectory::rename("tmp/f1", "tmp/f2")` → 分发到 ramfs 根目录
2. `DirNode::rename("tmp/f1", "tmp/f2")`：
   - `split_path("tmp/f1")` → `("tmp", Some("f1"))`
   - `split_path("tmp/f2")` → `("tmp", Some("f2"))`
   - `src_rest = Some("f1")`，进入递归分支
3. 递归调用 `child.rename("f1", "tmp/f2")`：
   - `split_path("f1")` → `("f1", None)`
   - `split_path("tmp/f2")` → `("tmp", Some("f2"))`
   - `src_rest = None`，进入叶子层
   - 但 `dst_rest = Some("f2")`，触发"跨目录重命名"错误！

### 5.2 根因分析

当递归进入子目录 `tmp` 时，目标路径 `"tmp/f2"` 的首组件 `"tmp"` 是当前递归层级的目录名，应当被剥离。未剥离导致叶子层误判为跨目录操作。

### 5.3 修复方案

在递归分支中，增加路径首组件匹配检查：若目标路径的首组件与源路径的首组件相同（即 `src_name == dst_name`），说明两者进入了同一子目录，此时剥离目标路径的首组件，将剩余部分作为递归调用的目标路径。

```rust
let dst = if src_name == dst_name {
    dst_rest.ok_or(VfsError::Unsupported)?
} else {
    return Err(VfsError::Unsupported);
};
return child.rename(src_rest, dst);
```

修复后的调用过程：

1. `DirNode::rename("tmp/f1", "tmp/f2")`：
   - `src_name = "tmp"`, `dst_name = "tmp"` → 匹配
   - `dst = dst_rest = "f2"`
2. 递归调用 `child.rename("f1", "f2")`：
   - `src_rest = None`，进入叶子层
   - `dst_rest = None`，正常执行重命名 ✓

---

## 6. 实验结果

在 riscv64 架构上通过 QEMU 成功构建并运行，输出如下：

```
Create directory '/tmp' ...
Create '/tmp/f1' and write [hello] ...
Read '/tmp/f1' content: [hello] ok!
Rename '/tmp/f1' to '/tmp/f2' ...
Read '/tmp/f2' content: [hello] ok!

[Ramfs-Rename]: ok!
```

输出表明：

1. ✅ 目录 `/tmp` 创建成功
2. ✅ 文件 `/tmp/f1` 创建并写入 `"hello"` 成功
3. ✅ 读取 `/tmp/f1` 内容为 `"hello"`，验证写入正确
4. ✅ 重命名 `/tmp/f1` → `/tmp/f2` 成功
5. ✅ 读取 `/tmp/f2` 内容仍为 `"hello"`，验证重命名后数据完整
6. ✅ 程序输出 `[Ramfs-Rename]: ok!`，满足成功标准

---

## 7. 总结

本实验通过为 ArceOS 的 ramfs 添加 `rename` 操作支持，深入理解了以下内容：

1. **VFS 层设计**：通过 [`VfsNodeOps`](axfs_vfs/src/lib.rs:87) trait 定义统一接口，不同文件系统实现各自的具体逻辑，实现了接口与实现的解耦

2. **Trait 默认方法**：Rust trait 的默认实现（如 `rename` 返回 `Unsupported`）允许渐进式实现——只需在需要的类型上覆写特定方法，而不影响其他类型

3. **组合模式的分发机制**：[`RootDirectory`](axfs/src/root.rs:46) 作为组合节点，通过 [`normalize_path()`](axfs/src/root.rs:108) 和 [`find_best_mount()`](axfs/src/root.rs:119) 将操作分发到正确的挂载文件系统，对上层调用者透明

4. **递归目录操作中的路径处理**：在递归遍历目录树时，源路径和目标路径必须同步剥离已遍历的路径组件，否则会导致路径语义错误。这是实现 `rename` 时最易出错的关键点
