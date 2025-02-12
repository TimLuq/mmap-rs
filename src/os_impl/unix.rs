use bitflags::bitflags;
use crate::{MmapFlags, PageSize, UnsafeMmapFlags};
use crate::error::Error;
use nix::sys::mman::*;
use nix::unistd::*;
use std::fs::File;
use std::ops::Range;
use std::os::unix::io::AsRawFd;

#[cfg(target_os = "ios")]
extern "C" {
    fn sys_icache_invalidate(start: *mut core::ffi::c_void, size: usize);
}

#[cfg(not(target_os = "ios"))]
extern "C" {
    /// This function is provided by LLVM to clear the instruction cache for the specified range.
    fn __clear_cache(start: *mut core::ffi::c_void, end: *mut core::ffi::c_void);
}

bitflags! {
    struct Flags: u32 {
        const JIT = 1 << 0;
    }
}

pub struct Mmap {
    file: Option<File>,
    ptr: *mut u8,
    size: usize,
    flags: Flags,
}

impl Mmap {
    #[inline]
    pub fn file(&self) -> Option<&File> {
        self.file.as_ref()
    }

    #[inline]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    #[inline]
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn lock(&mut self) -> Result<(), Error> {
        unsafe {
            mlock(
                self.ptr as *const std::ffi::c_void,
                self.size,
            )?;
        }

        Ok(())
    }

    pub fn unlock(&mut self) -> Result<(), Error> {
        unsafe {
            munlock(
                self.ptr as *const std::ffi::c_void,
                self.size,
            )?;
        }

        Ok(())
    }

    pub fn flush(&self, range: Range<usize>) -> Result<(), Error> {
        unsafe {
            msync(
                self.ptr.offset(range.start as isize) as *mut std::ffi::c_void,
                range.end - range.start,
                MsFlags::MS_SYNC,
            )
        }?;

        Ok(())
    }

    pub fn flush_async(&self, range: Range<usize>) -> Result<(), Error> {
        unsafe {
            msync(
                self.ptr.offset(range.start as isize) as *mut std::ffi::c_void,
                range.end - range.start,
                MsFlags::MS_ASYNC,
            )
        }?;

        Ok(())
    }

    #[cfg(target_os = "ios")]
    pub fn flush_icache(&self) -> Result<(), Error> {
        unsafe {
            sys_icache_invalidate(
                self.ptr as *mut std::ffi::c_void,
                self.size as usize,
            )
        };

        Ok(())
    }

    #[cfg(not(target_os = "ios"))]
    pub fn flush_icache(&self) -> Result<(), Error> {
        unsafe {
            __clear_cache(
                self.ptr as *mut std::ffi::c_void,
                self.ptr.offset(self.size as isize) as *mut std::ffi::c_void,
            )
        };

        Ok(())
    }

    fn do_make(&self, protect: ProtFlags) -> Result<(), Error> {
        let ptr  = self.ptr as *const u8;
        let size = self.size;

        unsafe {
            mprotect(
                ptr as *mut std::ffi::c_void,
                size,
                protect,
            )?;
        }

        Ok(())
    }

    pub fn make_none(&self) -> Result<(), Error> {
        self.do_make(ProtFlags::PROT_NONE)
    }

    pub fn make_read_only(&self) -> Result<(), Error> {
        self.do_make(ProtFlags::PROT_READ)
    }

    pub fn make_exec(&self) -> Result<(), Error> {
        self.do_make(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC)
    }

    pub fn make_mut(&self) -> Result<(), Error> {
        self.do_make(ProtFlags::PROT_READ | ProtFlags::PROT_WRITE)
    }

    pub fn make_exec_mut(&self) -> Result<(), Error> {
        if !self.flags.contains(Flags::JIT) {
            return Err(Error::UnsafeFlagNeeded(UnsafeMmapFlags::JIT));
        }

        self.do_make(ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC)
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        let _ = unsafe {
            munmap(
                self.ptr as *mut _,
                self.size,
            )
        };
    }
}

pub struct MmapOptions {
    address: Option<usize>,
    file: Option<(File, u64)>,
    size: usize,
    flags: MmapFlags,
    unsafe_flags: UnsafeMmapFlags,
    page_size: Option<PageSize>,
}

impl MmapOptions {
    pub fn new(size: usize) -> Self {
        Self {
            address: None,
            file: None,
            size,
            flags: MmapFlags::empty(),
            unsafe_flags: UnsafeMmapFlags::empty(),
            page_size: None,
        }
    }

    pub fn page_size() -> (usize, usize) {
        let status = sysconf(SysconfVar::PAGE_SIZE);

        let size = match status {
            Ok(Some(page_size)) => page_size as usize,
            _ => 4096,
        };

        (size, size)
    }

    pub fn with_address(mut self, address: usize) -> Self {
        self.address = Some(address);
        self
    }

    pub fn with_file(mut self, file: File, offset: u64) -> Self {
        self.file = Some((file, offset));
        self
    }

    pub fn with_flags(mut self, flags: MmapFlags) -> Self {
        self.flags = flags;
        self
    }

    pub fn with_unsafe_flags(mut self, flags: UnsafeMmapFlags) -> Self {
        self.unsafe_flags = flags;
        self
    }

    pub fn with_page_size(mut self, page_size: PageSize) -> Self {
        self.page_size = Some(page_size);
        self
    }

    fn flags(&self) -> MapFlags {
        let mut flags = MapFlags::empty();

        if self.file.is_none() {
            flags |= MapFlags::MAP_ANONYMOUS;
        }

        flags |= if self.flags.contains(MmapFlags::COPY_ON_WRITE) {
            MapFlags::MAP_PRIVATE
        } else {
            MapFlags::MAP_SHARED
        };

        #[cfg(any(target_os = "android", target_os = "linux"))]
        if self.flags.contains(MmapFlags::POPULATE) {
            flags |= MapFlags::MAP_POPULATE;
        }

        #[cfg(not(any(target_os = "dragonfly", target_os = "freebsd")))]
        if self.flags.contains(MmapFlags::NO_RESERVE) {
            flags |= MapFlags::MAP_NORESERVE;
        }

        #[cfg(any(target_os = "android", target_os = "linux"))]
        if self.flags.contains(MmapFlags::HUGE_PAGES) {
            flags |= MapFlags::MAP_HUGETLB;
        }

        #[cfg(any(target_os = "android", target_os = "linux"))]
        if let Some(page_size) = self.page_size {
            flags |= MapFlags::MAP_HUGETLB;

            flags |= match page_size {
                PageSize::_64K  => MapFlags::MAP_HUGE_64KB,
                PageSize::_512K => MapFlags::MAP_HUGE_512KB,
                PageSize::_1M   => MapFlags::MAP_HUGE_1MB,
                PageSize::_2M   => MapFlags::MAP_HUGE_2MB,
                PageSize::_8M   => MapFlags::MAP_HUGE_8MB,
                PageSize::_16M  => MapFlags::MAP_HUGE_16MB,
                PageSize::_32M  => MapFlags::MAP_HUGE_32MB,
                PageSize::_256M => MapFlags::MAP_HUGE_256MB,
                PageSize::_512M => MapFlags::MAP_HUGE_512MB,
                PageSize::_1G   => MapFlags::MAP_HUGE_1GB,
                PageSize::_2G   => MapFlags::MAP_HUGE_2GB,
                PageSize::_16G  => MapFlags::MAP_HUGE_16GB,
                _ => MapFlags::empty(),
            };
        }

        #[cfg(any(target_os = "freebsd"))]
        if self.flags.contains(MmapFlags::HUGE_PAGES) {
            flags |= MapFlags::MAP_ALIGNED_SUPER;
        }

        #[cfg(any(target_os = "android", target_os = "linux"))]
        if self.flags.contains(MmapFlags::LOCKED) {
            flags |= MapFlags::MAP_LOCKED;
        }

        #[cfg(target_os = "netbsd")]
        if self.flags.contains(MmapFlags::LOCKED) {
            flags |= MapFlags::MAP_WIRED;
        }

        #[cfg(any(target_os = "android", target_os = "dragonfly", target_os = "freebsd", target_os = "linux", target_os = "openbsd"))]
        if self.flags.contains(MmapFlags::STACK) {
            flags |= MapFlags::MAP_STACK;
        }

        #[cfg(target_os = "openbsd")]
        if self.flags.contains(MmapFlags::NO_CORE_DUMP) {
            flags |= MapFlags::MAP_CONCEAL;
        }

        if self.unsafe_flags.contains(UnsafeMmapFlags::MAP_FIXED) {
            flags |= MapFlags::MAP_FIXED;
        }

        #[cfg(any(target_os = "ios", target_os = "macos"))]
        if self.unsafe_flags.contains(UnsafeMmapFlags::JIT) {
            flags |= MapFlags::MAP_JIT;
        }

        flags
    }

    fn do_map(self, protect: ProtFlags) -> Result<Mmap, Error> {
        let size = self.size;
        let ptr = unsafe {
            mmap(
                self.address
                    .map(|address| address as *mut std::ffi::c_void)
                    .unwrap_or(std::ptr::null_mut()),
                size,
                protect,
                self.flags(),
                self.file
                    .as_ref()
                    .map(|(file, _)| file.as_raw_fd())
                    .unwrap_or(-1),
                self.file
                    .as_ref()
                    .map(|(_, offset)| *offset as _)
                    .unwrap_or(0),
            )
        }?;

        #[cfg(any(target_os = "android", target_os = "linux"))]
        if self.flags.contains(MmapFlags::NO_CORE_DUMP) {
            unsafe {
                madvise(
                    ptr,
                    size,
                    MmapAdvise::MADV_DONTDUMP,
                )
            }?;
        }

        #[cfg(any(target_os = "dragonfly", target_os = "freebsd"))]
        if self.flags.contains(MmapFlags::NO_CORE_DUMP) {
            unsafe {
                madvise(
                    ptr,
                    size,
                    MmapAdvise::MADV_NOCORE,
                )
            }?;
        }

        #[cfg(not(any(target_os = "android", target_os = "linux", target_os = "netbsd")))]
        if self.flags.contains(MmapFlags::LOCKED) {
            unsafe {
                mlock(
                    ptr,
                    size,
                )
            }?;
        }

        let mut flags = Flags::empty();

        if self.unsafe_flags.contains(UnsafeMmapFlags::JIT) {
            flags |= Flags::JIT;
        }

        Ok(Mmap {
            file: self.file.map(|(file, _)| file),
            ptr: ptr as *mut u8,
            size,
            flags,
        })
    }

    pub fn map_none(self) -> Result<Mmap, Error> {
        self.do_map(ProtFlags::PROT_NONE)
    }

    pub fn map(self) -> Result<Mmap, Error> {
        self.do_map(ProtFlags::PROT_READ)
    }

    pub fn map_exec(self) -> Result<Mmap, Error> {
        self.do_map(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC)
    }

    pub fn map_mut(self) -> Result<Mmap, Error> {
        self.do_map(ProtFlags::PROT_READ | ProtFlags::PROT_WRITE)
    }

    pub fn map_exec_mut(self) -> Result<Mmap, Error> {
        if !self.unsafe_flags.contains(UnsafeMmapFlags::JIT) {
            return Err(Error::UnsafeFlagNeeded(UnsafeMmapFlags::JIT));
        }

        self.do_map(ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC)
    }
}
