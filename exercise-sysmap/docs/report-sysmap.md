# exercise-sysmap 实验报告

## 一、代码框架

本项目是基于 ArceOS (v0.3.0-preview.3) 的最小化内核，在 QEMU 模拟器上运行 musl 静态链接的 Linux 用户程序。架构分层如下：

```
┌─────────────────────────────────────────────────┐
│              用户空间 (musl ELF)                  │
│  payload/mapfile_c/mapfile.c → /sbin/mapfile    │
├─────────────────────────────────────────────────┤
│           系统调用模拟层                          │
│  src/syscall.rs (handle_syscall → sys_mmap 等)   │
├─────────────────────────────────────────────────┤
│              任务与异常处理                        │
│  src/task.rs (spawn_user_task, 异常循环)          │
├─────────────────────────────────────────────────┤
│              ELF 加载器                           │
│  src/loader.rs (load_user_app, PT_LOAD)          │
├─────────────────────────────────────────────────┤
│              内核主函数                            │
│  src/main.rs (USER_ASPACE, 初始化, spawn)         │
├─────────────────────────────────────────────────┤
│         ArceOS 组件库 (axstd 生态)                │
│  axmm (分页) │ axhal (用户态陷阱) │ axfs          │
│  axtask      │ axsync (Mutex)    │ axlog         │
├─────────────────────────────────────────────────┤
│         硬件 (QEMU virt/q35 机器)                 │
│  VirtIO-Blk (FAT32) │ UART │ Timer │ Interrupt   │
└─────────────────────────────────────────────────┘
```

### 核心文件说明

| 文件 | 功能 |
|------|------|
| `src/main.rs` | 内核入口，创建用户地址空间 `[0x0, 0x40_0000_0000)`、加载 ELF、映射用户栈、启动用户任务。定义全局 `USER_ASPACE: Mutex<Option<Arc<Mutex<AddrSpace>>>>` 供系统调用层访问用户地址空间 |
| `src/loader.rs` | ELF64 加载器，从 FAT32 文件系统读取用户程序，遍历 PT_LOAD 段并映射到用户地址空间，零填充 BSS 段 |
| `src/syscall.rs` | Linux 系统调用模拟层，处理 read/write/openat/close/mmap/brk/writev/ioctl/set_tid_address/arch_prctl/exit/exit_group 等系统调用。包含架构相关的系统调用号定义、FD 表、MmapProt/MmapFlags 位标志定义 |
| `src/task.rs` | 用户任务创建与异常分发循环，通过 `UserContext::run()` 进入用户态，Syscall 返回时调用 `handle_syscall()`，Interrupt 返回时重新进入用户态 |
| `payload/mapfile_c/mapfile.c` | C 语言用户态测试程序，创建文件写入 "hello, arceos!"，然后 mmap 映射文件并读取内容 |
| `xtask/src/main.rs` | 构建编排工具：交叉编译 payload → 创建 FAT32 磁盘镜像 → 构建内核 → 启动 QEMU |

### 启动流程

1. ArceOS 初始化硬件、文件系统（VirtIO-Blk FAT32），调用 `main()`
2. `main()` 创建用户地址空间，调用 `loader::load_user_app("/sbin/mapfile", &mut uspace)` 加载 ELF
3. 映射用户栈（64KiB），准备 argv/envp/auxv 并写入用户空间
4. 将地址空间存入全局 `USER_ASPACE`
5. 调用 `task::spawn_user_task()` 创建内核任务，进入用户态
6. 用户态发生系统调用时，陷入内核态，由 `syscall::handle_syscall()` 分发处理

## 二、实验内容

实验要求实现 `SYS_MMAP` 系统调用（即 Linux 的 `mmap(2)`），使得用户程序能够通过内存映射的方式读取文件内容。

### 用户态测试程序分析

`payload/mapfile_c/mapfile.c` 的执行流程：

1. `create_file()`: 创建文件 `"test_file"`，写入 `"hello, arceos!\0"`（16 字节），关闭文件
2. `verify_file()`: 以只读方式打开 `"test_file"`，调用 `mmap(NULL, 32, PROT_READ, MAP_PRIVATE, fd, 0)` 映射文件
3. 从映射地址读取内容并打印：`Read back content: hello, arceos!`
4. 打印 `MapFile ok!`

关键系统调用序列：
```c
addr = mmap(NULL, 32, PROT_READ, MAP_PRIVATE, fd, 0);
if (addr == NULL) {
    printf("Map file error!\n");
    return;
}
printf("Read back content: %s\n", (char *)addr);
```

这是一个**文件私有映射**：`MAP_PRIVATE` 表示写时复制（COW），`PROT_READ` 表示只读，`fd` 是已打开的文件描述符，`offset` 为 0。

### 验证标准

串口输出必须包含：
- `Read back content: hello, arceos!`
- `MapFile ok!`

## 三、实现方法

### 原始代码

`src/syscall.rs` 中的 `sys_mmap()` 函数原本是：
```rust
fn sys_mmap(
    _addr: *mut c_void,
    _length: usize,
    _prot: i32,
    _flags: i32,
    _fd: i32,
    _offset: isize,
) -> isize {
    unimplemented!("no sys_mmap!");
}
```

### 实现步骤

#### 1. 参数解析

- 将 `prot` 解析为 `MmapProt` 位标志（`PROT_READ`/`PROT_WRITE`/`PROT_EXEC`）
- 将 `flags` 解析为 `MmapFlags` 位标志（`MAP_SHARED`/`MAP_PRIVATE`/`MAP_ANONYMOUS` 等）
- 通过已有的 `From<MmapProt> for MappingFlags` 转换获取 ArceOS 映射权限（自动添加 `USER` 位）

#### 2. 长度对齐

将 `length` 向上取整到 `PAGE_SIZE_4K`（4096 字节）的整数倍：
```rust
let length_aligned = (length + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
```

#### 3. NULL 页保护

将 hint 地址钳位到 `max(PAGE_SIZE_4K)`，确保 `find_free_area()` 不会从地址 0 开始搜索：
```rust
let hint = VirtAddr::from(hint.as_usize().max(PAGE_SIZE_4K));
```

这模拟了 Linux 内核的行为：`mmap` 永远不会返回地址 0（NULL 页始终被排除）。如果不做此处理，`find_free_area()` 会从地址空间基址 `0x0` 开始搜索，找到 ELF 段之前的空闲区域（地址 0），导致返回 NULL，用户程序判定为错误。

#### 4. 匿名映射（MAP_ANONYMOUS）

当 `flags` 包含 `MAP_ANONYMOUS` 时：
- 调用 `uspace.find_free_area()` 找到可用虚拟地址区域
- 调用 `uspace.map_alloc()` 映射物理页面
- 返回映射的虚拟地址

#### 5. 文件映射（核心路径）

当 `fd` 有效且非匿名映射时：
1. 通过 `crate::USER_ASPACE` 获取用户地址空间
2. 调用 `uspace.find_free_area()` 定位空闲虚拟地址
3. 调用 `uspace.map_alloc(start, length_aligned, mapping_flags, true)` 分配并映射物理页面（`populate=true` 立即分配物理页）
4. 通过 `with_file_fd()` 访问文件描述符，使用 `file.read_at(offset, buf)` 读取文件内容
5. 调用 `uspace.write(start, &buf[..n])` 将文件数据写入映射的用户内存
6. 返回映射的虚拟地址

#### 6. 错误处理

- 参数无效（length 为 0 等）：返回 `neg_errno(LinuxError::EINVAL)`
- 内存不足：返回 `neg_errno(LinuxError::ENOMEM)`
- IO 错误：返回 `neg_errno(LinuxError::EIO)`
- 文件描述符无效：返回 `neg_errno(LinuxError::EBADF)`

### 关键设计决策

1. **使用 `find_free_area()` + `map_alloc()` 而非仅 `map_alloc()`**：`map_alloc()` 需要指定起始地址（不会自动选择空闲区域），因此需要先用 `find_free_area()` 找到合适的虚拟地址范围。

2. **`populate=true` 参数**：确保 `map_alloc()` 立即分配物理页面，而非延迟分配（lazy allocation）。这对于文件映射是必要的，因为后续需要立即写入文件数据。

3. **使用 `read_at()` 而非 `read()`**：文件可能已被读取过（如 ELF 加载时），文件偏移可能不在 0，因此使用 `read_at(offset)` 确保从正确的偏移量读取。

4. **NULL 页保护**：Linux 内核永远不会从 mmap 返回地址 0，这是防止 NULL 指针解引用的重要保护机制。在 ArceOS 中，由于用户地址空间从 0 开始，需要手动跳过地址 0。

## 四、验证结果

✅ **QEMU 运行时测试通过**（riscv64 架构）

串口输出包含预期的两行：
```
Read back content: hello, arceos!
MapFile ok!
```

内核正常退出：`monolithic kernel exit [Some(0)] normally!`

### 调试过程

初次实现时，测试输出为 `Map file error!`，原因是 `sys_mmap()` 返回了地址 0。分析发现：
- 用户地址空间基址为 `0x0`
- `find_free_area()` 从 `0x0` 开始搜索，找到 ELF 段（`0x10000`）之前的空闲区域
- `map_alloc(0x0, ...)` 成功映射到地址 0
- C 代码 `if (addr == NULL)` 判定为错误

修复方法：将 hint 地址钳位到 `max(PAGE_SIZE_4K)`，跳过 NULL 页。

## 五、总结

本实验通过实现 `sys_mmap()` 系统调用，深入理解了以下概念：

1. **虚拟内存管理**：页面映射、地址空间操作、物理页面分配
2. **mmap 语义**：文件映射与匿名映射的区别、保护标志与映射标志的含义
3. **用户态-内核态交互**：系统调用参数传递、返回值约定、错误码处理
4. **ELF 加载与用户程序执行**：用户地址空间布局、栈初始化、异常分发循环
5. **NULL 页保护**：操作系统必须防止 mmap 返回地址 0，这是基本的安全机制
