//! Linux syscall emulation for musl user programs (open/read/write/mmap/…).
//!
//! Syscall numbers differ by architecture; see `nums` modules below.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicUsize, Ordering};

use axerrno::LinuxError;
use axfs::fops::{File, OpenOptions};
use axhal::paging::MappingFlags;
use axhal::uspace::UserContext;
use axsync::Mutex;

// ---- Architecture-specific syscall numbers ----

#[cfg(not(target_arch = "x86_64"))]
mod nums {
    pub const SYS_IOCTL: usize = 29;
    pub const SYS_WRITEV: usize = 66;
    pub const SYS_READ: usize = 63;
    pub const SYS_WRITE: usize = 64;
    pub const SYS_OPENAT: usize = 56;
    pub const SYS_CLOSE: usize = 57;
    pub const SYS_EXIT: usize = 93;
    pub const SYS_EXIT_GROUP: usize = 94;
    pub const SYS_SET_TID_ADDRESS: usize = 96;
    pub const SYS_MMAP: usize = 222;
    pub const SYS_BRK: usize = 214;
}

#[cfg(target_arch = "x86_64")]
mod nums {
    /// Legacy `open(2)`; musl may use this on x86_64 instead of `openat`.
    pub const SYS_OPEN: usize = 2;
    pub const SYS_IOCTL: usize = 16;
    pub const SYS_WRITEV: usize = 20;
    pub const SYS_READ: usize = 0;
    pub const SYS_WRITE: usize = 1;
    pub const SYS_OPENAT: usize = 257;
    pub const SYS_CLOSE: usize = 3;
    pub const SYS_EXIT: usize = 60;
    pub const SYS_EXIT_GROUP: usize = 231;
    pub const SYS_SET_TID_ADDRESS: usize = 218;
    pub const SYS_MMAP: usize = 9;
    pub const SYS_BRK: usize = 12;
    pub const SYS_ARCH_PRCTL: usize = 158;
    pub const ARCH_SET_FS: usize = 0x1002;
}

use nums::*;

const AT_FDCWD: i32 = -100;

// Linux open(2) flags (musl / kernel ABI)
const O_ACCMODE: u32 = 0o3;
const O_RDONLY: u32 = 0o0;
const O_WRONLY: u32 = 0o1;
const O_RDWR: u32 = 0o2;
const O_CREAT: u32 = 0o100;
const O_TRUNC: u32 = 0o1000;
const O_APPEND: u32 = 0o2000;
const O_EXCL: u32 = 0o200;

/// Program break for minimal `brk` emulation.
static PROGRAM_BRK: AtomicUsize = AtomicUsize::new(0x0300_0000);

static FD_TABLE: Mutex<Vec<Option<File>>> = Mutex::new(Vec::new());

#[repr(C)]
struct IoVec {
    iov_base: usize,
    iov_len: usize,
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// permissions for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapProt: i32 {
        /// Page can be read.
        const PROT_READ = 1 << 0;
        /// Page can be written.
        const PROT_WRITE = 1 << 1;
        /// Page can be executed.
        const PROT_EXEC = 1 << 2;
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapFlags: i32 {
        /// Share changes
        const MAP_SHARED = 1 << 0;
        /// Changes private; copy pages on write.
        const MAP_PRIVATE = 1 << 1;
        /// Map address must be exactly as requested, no matter whether it is available.
        const MAP_FIXED = 1 << 4;
        /// Don't use a file.
        const MAP_ANONYMOUS = 1 << 5;
        /// Don't check for reservations.
        const MAP_NORESERVE = 1 << 14;
        /// Allocation is for a stack.
        const MAP_STACK = 0x20000;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::PROT_READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::PROT_WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::PROT_EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

fn get_syscall_num(uctx: &UserContext) -> usize {
    #[cfg(any(
        target_arch = "riscv64",
        target_arch = "riscv32",
        target_arch = "loongarch64"
    ))]
    {
        uctx.regs.a7 as usize
    }
    #[cfg(target_arch = "aarch64")]
    {
        uctx.x[8] as usize
    }
    #[cfg(target_arch = "x86_64")]
    {
        uctx.rax as usize
    }
}

fn set_syscall_ret(uctx: &mut UserContext, ret: usize) {
    #[cfg(target_arch = "x86_64")]
    {
        uctx.rax = ret as u64;
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        uctx.set_arg0(ret);
    }
}

fn neg_errno(e: LinuxError) -> isize {
    -(e.code() as isize)
}

unsafe fn c_str_to_string(ptr: *const c_char) -> Result<String, LinuxError> {
    if ptr.is_null() {
        return Err(LinuxError::EFAULT);
    }
    let mut len = 0usize;
    loop {
        let b = unsafe { *ptr.add(len) };
        if b == 0 {
            break;
        }
        len += 1;
        if len > 4096 {
            return Err(LinuxError::ENAMETOOLONG);
        }
    }
    let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    let s = core::str::from_utf8(slice).map_err(|_| LinuxError::EINVAL)?;
    Ok(String::from(s))
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        String::from(path)
    } else {
        alloc::format!("/{path}")
    }
}

fn linux_flags_to_open_options(flags: u32, _mode: u32) -> Result<OpenOptions, LinuxError> {
    let acc = flags & O_ACCMODE;
    let mut opts = OpenOptions::new();
    match acc {
        O_RDONLY => opts.read(true),
        O_WRONLY => opts.write(true),
        O_RDWR => {
            opts.read(true);
            opts.write(true);
        }
        _ => return Err(LinuxError::EINVAL),
    }
    if flags & O_APPEND != 0 {
        opts.append(true);
    }
    if flags & O_TRUNC != 0 {
        opts.truncate(true);
    }
    if flags & O_CREAT != 0 {
        opts.create(true);
    }
    if flags & O_EXCL != 0 {
        opts.create_new(true);
    }
    Ok(opts)
}

fn fd_alloc(file: File) -> i32 {
    let mut t = FD_TABLE.lock();
    for (i, slot) in t.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(file);
            return i as i32;
        }
    }
    t.push(Some(file));
    (t.len() - 1) as i32
}

fn fd_take(fd: i32) -> Result<File, LinuxError> {
    if fd < 0 {
        return Err(LinuxError::EBADF);
    }
    let i = fd as usize;
    let mut t = FD_TABLE.lock();
    if i >= t.len() {
        return Err(LinuxError::EBADF);
    }
    t[i].take().ok_or(LinuxError::EBADF)
}

// Index-based access with a closure (holds `FD_TABLE` only for the duration of `f`).

fn with_file_fd<F, R>(fd: i32, f: F) -> Result<R, LinuxError>
where
    F: FnOnce(&mut File) -> Result<R, LinuxError>,
{
    if fd < 0 {
        return Err(LinuxError::EBADF);
    }
    let i = fd as usize;
    let mut t = FD_TABLE.lock();
    let slot = t.get_mut(i).ok_or(LinuxError::EBADF)?;
    let file = slot.as_mut().ok_or(LinuxError::EBADF)?;
    f(file)
}

fn sys_openat(dfd: c_int, fname: *const c_char, flags: c_int, mode: u32) -> isize {
    if dfd != AT_FDCWD {
        return neg_errno(LinuxError::EINVAL);
    }
    let path = match unsafe { c_str_to_string(fname) } {
        Ok(s) => normalize_path(&s),
        Err(e) => return neg_errno(e),
    };
    let flags = flags as u32;
    let opts = match linux_flags_to_open_options(flags, mode) {
        Ok(o) => o,
        Err(e) => return neg_errno(e),
    };
    match File::open(path.as_str(), &opts) {
        Ok(f) => {
            if f.get_attr().map(|a| a.is_dir()).unwrap_or(false) {
                neg_errno(LinuxError::EISDIR)
            } else {
                fd_alloc(f) as isize
            }
        }
        Err(e) => neg_errno(LinuxError::from(e)),
    }
}

fn sys_close(fd: i32) -> isize {
    match fd_take(fd) {
        Ok(_file) => 0,
        Err(e) => neg_errno(e),
    }
}

fn sys_read(fd: i32, buf: *mut c_void, count: usize) -> isize {
    if buf.is_null() {
        return neg_errno(LinuxError::EFAULT);
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count) };
    match with_file_fd(fd, |file| match file.read(slice) {
        Ok(n) => Ok(n as isize),
        Err(e) => Err(LinuxError::from(e)),
    }) {
        Ok(n) => n,
        Err(e) => neg_errno(e),
    }
}

fn sys_write(fd: i32, buf: *const c_void, count: usize) -> isize {
    if fd == 1 || fd == 2 {
        if buf.is_null() {
            return neg_errno(LinuxError::EFAULT);
        }
        let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
        for &b in slice {
            ax_print!("{}", b as char);
        }
        return count as isize;
    }
    if buf.is_null() {
        return neg_errno(LinuxError::EFAULT);
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
    match with_file_fd(fd, |file| match file.write(slice) {
        Ok(n) => Ok(n as isize),
        Err(e) => Err(LinuxError::from(e)),
    }) {
        Ok(n) => n,
        Err(e) => neg_errno(e),
    }
}

fn sys_writev(fd: i32, iov: *const IoVec, iovcnt: i32) -> isize {
    if fd != 1 && fd != 2 {
        return neg_errno(LinuxError::EBADF);
    }
    let mut total: isize = 0;
    for i in 0..iovcnt as usize {
        let entry = unsafe { &*iov.add(i) };
        if entry.iov_len == 0 || entry.iov_base == 0 {
            continue;
        }
        let slice =
            unsafe { core::slice::from_raw_parts(entry.iov_base as *const u8, entry.iov_len) };
        for &b in slice {
            ax_print!("{}", b as char);
        }
        total += entry.iov_len as isize;
    }
    total
}

fn sys_brk(addr: usize) -> isize {
    // Linux brk syscall returns the new program break on success.
    let cur = PROGRAM_BRK.load(Ordering::Relaxed);
    if addr == 0 {
        return cur as isize;
    }
    if addr < cur {
        return neg_errno(LinuxError::ENOMEM);
    }
    PROGRAM_BRK.store(addr, Ordering::Relaxed);
    addr as isize
}

fn sys_mmap(
    addr: *mut c_void,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: isize,
) -> isize {
    use alloc::sync::Arc;
    use memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};

    let mmap_flags = MmapFlags::from_bits_truncate(flags);
    let mmap_prot = MmapProt::from_bits_truncate(prot);
    let mapping_flags: MappingFlags = mmap_prot.into();

    if length == 0 {
        return neg_errno(LinuxError::EINVAL);
    }

    // Round length up to page size
    let length_aligned = (length + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);

    // Get user address space: USER_ASPACE is Mutex<Option<Arc<Mutex<AddrSpace>>>>
    let uspace_arc = {
        let outer = crate::USER_ASPACE.lock();
        match outer.as_ref() {
            Some(arc) => Arc::clone(arc),
            None => return neg_errno(LinuxError::ENOMEM),
        }
    };

    let mut uspace = uspace_arc.lock();

    // Find a free area for the mapping
    let hint = if addr.is_null() {
        uspace.base()
    } else {
        VirtAddr::from(addr as usize)
    };
    // Never allow mmap to return address 0 — skip the NULL page
    let hint = VirtAddr::from(hint.as_usize().max(PAGE_SIZE_4K));
    let limit = VirtAddrRange::from_start_size(uspace.base(), uspace.size());
    let start = match uspace.find_free_area(hint, length_aligned, limit) {
        Some(va) => va,
        None => return neg_errno(LinuxError::ENOMEM),
    };

    // Allocate and map memory
    if uspace
        .map_alloc(start, length_aligned, mapping_flags, true)
        .is_err()
    {
        return neg_errno(LinuxError::ENOMEM);
    }

    // Handle file-backed mapping (non-anonymous with valid fd)
    if !mmap_flags.contains(MmapFlags::MAP_ANONYMOUS) && fd >= 0 {
        if offset < 0 {
            return neg_errno(LinuxError::EINVAL);
        }
        let mut buf = alloc::vec![0u8; length];
        let read_result = with_file_fd(fd, |file| {
            match file.read_at(offset as u64, &mut buf) {
                Ok(n) => Ok(n),
                Err(e) => Err(LinuxError::from(e)),
            }
        });
        match read_result {
            Ok(n) => {
                if n > 0 {
                    if uspace.write(start, &buf[..n]).is_err() {
                        return neg_errno(LinuxError::EIO);
                    }
                }
            }
            Err(e) => return neg_errno(e),
        }
    }

    start.as_usize() as isize
}

#[cfg(target_arch = "x86_64")]
fn sys_arch_prctl(uctx: &mut UserContext, op: usize, addr: usize) -> isize {
    match op {
        ARCH_SET_FS => {
            uctx.fs_base = addr as u64;
            0
        }
        _ => {
            ax_println!("Unimplemented arch_prctl op: {:#x}", op);
            neg_errno(LinuxError::EINVAL)
        }
    }
}

/// Handle a syscall from user space.
pub fn handle_syscall(uctx: &mut UserContext) -> Option<i32> {
    let syscall_num = get_syscall_num(uctx);
    let args = [
        uctx.arg0(),
        uctx.arg1(),
        uctx.arg2(),
        uctx.arg3(),
        uctx.arg4(),
        uctx.arg5(),
    ];
    ax_println!("handle_syscall [{}] ...", syscall_num);

    let ret: isize = match syscall_num {
        SYS_IOCTL => {
            ax_println!("Ignore SYS_IOCTL");
            0
        }
        SYS_SET_TID_ADDRESS => axtask::current().id().as_u64() as isize,
        SYS_BRK => sys_brk(args[0]),
        #[cfg(target_arch = "x86_64")]
        SYS_OPEN => sys_openat(
            AT_FDCWD,
            args[0] as *const c_char,
            args[1] as c_int,
            args[2] as u32,
        ),
        SYS_OPENAT => sys_openat(
            args[0] as c_int,
            args[1] as *const c_char,
            args[2] as c_int,
            args[3] as u32,
        ),
        SYS_CLOSE => sys_close(args[0] as i32),
        SYS_READ => sys_read(args[0] as i32, args[1] as *mut c_void, args[2]),
        SYS_WRITE => sys_write(args[0] as i32, args[1] as *const c_void, args[2]),
        SYS_WRITEV => sys_writev(args[0] as i32, args[1] as *const IoVec, args[2] as i32),
        SYS_EXIT_GROUP => {
            ax_println!("[SYS_EXIT_GROUP]: exiting ..");
            return Some(args[0] as i32);
        }
        SYS_EXIT => {
            ax_println!("[SYS_EXIT]: exiting ..");
            return Some(args[0] as i32);
        }
        SYS_MMAP => sys_mmap(
            args[0] as *mut c_void,
            args[1],
            args[2] as i32,
            args[3] as i32,
            args[4] as i32,
            args[5] as isize,
        ),
        #[cfg(target_arch = "x86_64")]
        SYS_ARCH_PRCTL => sys_arch_prctl(uctx, args[0], args[1]),
        _ => {
            ax_println!("Unimplemented syscall: {}", syscall_num);
            neg_errno(LinuxError::ENOSYS)
        }
    };

    set_syscall_ret(uctx, ret as usize);
    None
}
