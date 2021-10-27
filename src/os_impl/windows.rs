windows::include_bindings!();

use bitflags::bitflags;
use crate::{MmapFlags, PageSize, UnsafeMmapFlags};
use crate::error::Error;
use std::fs::File;
use std::ops::Range;
use std::os::windows::io::AsRawHandle;
use windows::Handle;
use Windows::Win32::Foundation::{CloseHandle, HANDLE, PWSTR};
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
use Windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;    
use Windows::Win32::System::Memory::*;
use Windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
use Windows::Win32::System::Threading::GetCurrentProcess;

bitflags! {
    struct Flags: u32 {
        const COPY_ON_WRITE = 1 << 0;
        const JIT           = 1 << 1;
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
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn lock(&mut self) -> Result<(), Error> {
        let status = unsafe {
            VirtualLock(
                self.ptr as *const std::ffi::c_void,
                self.size,
            )
        }.as_bool();

        if !status {
            return Err(std::io::Error::last_os_error())?;
        }

        Ok(())
    }

    pub fn unlock(&mut self) -> Result<(), Error> {
        let status = unsafe {
            VirtualUnlock(
                self.ptr as *const std::ffi::c_void,
                self.size,
            )
        }.as_bool();

        if !status {
            return Err(std::io::Error::last_os_error())?;
        }

        Ok(())
    }

    pub fn flush(&self, range: Range<usize>) -> Result<(), Error> {
        self.flush_async(range)?;

        if let Some(ref file) = self.file {
            file.sync_data()?;
        }

        Ok(())
    }

    pub fn flush_async(&self, range: Range<usize>) -> Result<(), Error> {
        if range.end <= range.start {
            return Ok(());
        }

        let status = unsafe {
            FlushViewOfFile(
                self.ptr.offset(range.start as isize) as *const std::ffi::c_void,
                range.end - range.start,
            )
        }.as_bool();

        if !status {
            return Err(std::io::Error::last_os_error())?;
        }

        Ok(())
    }

    pub fn do_make(&self, protect: PAGE_PROTECTION_FLAGS) -> Result<(), Error> {
        let mut old_protect = PAGE_PROTECTION_FLAGS::default();

        let status = unsafe {
            VirtualProtect(
                self.ptr as *mut std::ffi::c_void,
                self.size,
                protect,
                &mut old_protect,
            ).as_bool()
        };

       if !status {
           return Err(std::io::Error::last_os_error())?;
        }

        Ok(())
    }

    pub fn flush_icache(&self) -> Result<(), Error> {
        // While the x86 and x86-64 architectures guarantee cache coherency between the L1
        // instruction and the L1 data cache, other architectures such as arm and aarch64 do not.
        // If the user modified the pages, then executing the code after marking the pages as
        // executable may result in undefined behavior. Since we cannot efficiently track writes,
        // we have to flush the instruction cache unconditionally.
        #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
        unsafe {
            FlushInstructionCache(
                GetCurrentProcess(),
                self.ptr as *const std::ffi::c_void,
                self.size,
            )
        };

        Ok(())
    }

    pub fn make_none(&self) -> Result<(), Error> {
        self.do_make(PAGE_NOACCESS)
    }

    pub fn make_read_only(&self) -> Result<(), Error> {
        self.do_make(PAGE_READWRITE)
    }

    pub fn make_exec(&self) -> Result<(), Error> {
        self.do_make(PAGE_EXECUTE_READ)
    }

    pub fn make_mut(&self) -> Result<(), Error> {
        let protect = if self.flags.contains(Flags::COPY_ON_WRITE) {
            PAGE_WRITECOPY
        } else {
            PAGE_READWRITE
        };

        self.do_make(protect)
    }

    pub fn make_exec_mut(&self) -> Result<(), Error> {
        if !self.flags.contains(Flags::JIT) {
            return Err(Error::UnsafeFlagNeeded(UnsafeMmapFlags::JIT));
        }

        let protect = if self.flags.contains(Flags::COPY_ON_WRITE) {
            PAGE_EXECUTE_WRITECOPY
        } else {
            PAGE_EXECUTE_READWRITE
        };

        self.do_make(protect)
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.file.is_some() {
            let _ = unsafe {
                UnmapViewOfFile(
                    self.ptr as *mut _,
                )
            };
        } else {
            let _ = unsafe {
                VirtualFree(
                    self.ptr as *mut _,
                    self.size,
                    MEM_DECOMMIT | MEM_RELEASE,
                )
            };
        }
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
    pub fn new() -> Self {
        Self {
            address: None,
            file: None,
            size: 0,
            flags: MmapFlags::empty(),
            unsafe_flags: UnsafeMmapFlags::empty(),
            page_size: None,
        }
    }

    pub fn page_size() -> (usize, usize) {
        let mut system_info = SYSTEM_INFO::default();

        unsafe {
            GetSystemInfo(&mut system_info)
        };

        (system_info.dwPageSize as usize, system_info.dwAllocationGranularity as usize)
    }

    pub fn with_address(mut self, address: Option<usize>) -> Self {
        self.address = address;
        self
    }

    pub fn with_file(mut self, file: Option<(File, u64)>) -> Self {
        self.file = file;
        self
    }

    pub fn with_size(mut self, size: usize) -> Self {
        self.size = size;
        self
    }

    pub fn with_flags(mut self, flags: MmapFlags) -> Self {
        self.flags = flags;
        self
    }

    pub unsafe fn with_unsafe_flags(mut self, flags: UnsafeMmapFlags) -> Self {
        self.unsafe_flags = flags;
        self
    }

    pub fn with_page_size(mut self, page_size: Option<PageSize>) -> Self {
        self.page_size = page_size;
        self
    }

    /// This is a helper function that simply calls [`CreateFileMappingW`] and then [`CloseHandle`]
    /// to check if a file mapping can be created with the given protection. This is mostly needed
    /// to figure out whether a file mapping can be created with read, write and execute access.
    /// Returns true on success and false otherwise.
    fn check_protection(&self, protection: PAGE_PROTECTION_FLAGS) -> bool {
        // Grab a reference to the file, if there is one. Otherwise return false immediately.
        let file = match self.file.as_ref() {
            Some((file, _)) => file,
            _ => return false,
        };

        // Try creating a file mapping with the given protection.
        let file_mapping = unsafe {
            CreateFileMappingW(
                HANDLE(file.as_raw_handle() as isize),
                std::ptr::null_mut(),
                protection,
                0,
                0,
                PWSTR(std::ptr::null_mut()),
            )
        };

        // Return false if we could not create the mapping.
        if file_mapping.is_invalid() {
            return false;
        }

        // We could create the file mapping, now close the handle and return true.
        unsafe {
            CloseHandle(
                file_mapping,
            )
        };

        true
    }

    /// This is a helper function that goes through the process of setting up the desired memory
    /// mapping given the protection flag.
    fn do_map(mut self, protection: PAGE_PROTECTION_FLAGS) -> Result<Mmap, Error> {
        // We have to check whether we can create the file mapping with write and execute
        // permissions. As Microsoft Windows won't let us set any access flags other than those
        // that have been set initially, we have to figure out the full set of access flags that
        // we can set, and then narrow down the access rights to what the user requested.
        let write = self.check_protection(PAGE_READWRITE);
        let execute = self.check_protection(PAGE_EXECUTE_READ);

        let mut map_access = FILE_MAP_READ;
        let mut map_protection = match (write, execute) {
            (true, true) => {
                map_access |= FILE_MAP_WRITE | FILE_MAP_EXECUTE;
                PAGE_EXECUTE_READWRITE
            }
            (true, false) => {
                map_access |= FILE_MAP_WRITE;
                PAGE_READWRITE
            }
            (false, true) => {
                map_access |= FILE_MAP_EXECUTE;
                PAGE_EXECUTE_READ
            }
            (false, false) => PAGE_READONLY,
        };

        let size = self.size;
        let ptr = if let Some((file, offset)) = &self.file {
            if self.flags.contains(MmapFlags::HUGE_PAGES) {
                map_access |= FILE_MAP_LARGE_PAGES;
                map_protection |= SEC_LARGE_PAGES;
            }

            let file_mapping = unsafe {
                CreateFileMappingW(
                    HANDLE(file.as_raw_handle() as isize),
                    std::ptr::null_mut(),
                    map_protection,
                    ((size >> 32) & 0xffff_ffff) as u32,
                    (size & 0xffff_ffff) as u32,
                    PWSTR(std::ptr::null_mut()),
                )
            };

            let ptr = unsafe {
                MapViewOfFileEx(
                    file_mapping,
                    map_access,
                    ((offset >> 32) & 0xffff_ffff) as u32,
                    (offset & 0xffff_ffff) as u32,
                    size,
                    std::ptr::null(),
                )
            };

            unsafe {
                CloseHandle(file_mapping)
            };

            let mut old_protect = PAGE_PROTECTION_FLAGS::default();

            let status = unsafe {
                VirtualProtect(
                    ptr,
                    size,
                    protection,
                    &mut old_protect,
                )
            }.as_bool();

            if !status {
                return Err(std::io::Error::last_os_error())?;
            }

            ptr
        } else {
            let mut flags = MEM_COMMIT | MEM_RESERVE;

            if self.flags.contains(MmapFlags::HUGE_PAGES) {
                flags |= MEM_LARGE_PAGES;
            }

            unsafe {
                VirtualAlloc(
                    self.address
                        .map(|address| address as *mut std::ffi::c_void)
                        .unwrap_or(std::ptr::null_mut()),
                    size,
                    flags,
                    protection,
                )
            }
        };

        if ptr.is_null() {
            return Err(std::io::Error::last_os_error())?;
        }

        let size = self.size;
        let file = self.file.take().map(|(file, _)| file);
        let mut flags = Flags::empty();

        if self.flags.contains(MmapFlags::COPY_ON_WRITE) {
            flags |= Flags::COPY_ON_WRITE;
        }

        if self.unsafe_flags.contains(UnsafeMmapFlags::JIT) {
            flags |= Flags::JIT;
        }

        Ok(Mmap {
            file,
            ptr: ptr as *mut u8,
            size,
            flags,
        })
    }

    pub fn map_none(self) -> Result<Mmap, Error> {
        self.do_map(PAGE_NOACCESS)
    }

    pub fn map(self) -> Result<Mmap, Error> {
        self.do_map(PAGE_READONLY)
    }

    pub fn map_exec(self) -> Result<Mmap, Error> {
        self.do_map(PAGE_EXECUTE_READ)
    }

    pub fn map_mut(self) -> Result<Mmap, Error> {
        let protect = if self.flags.contains(MmapFlags::COPY_ON_WRITE) {
            PAGE_WRITECOPY
        } else {
            PAGE_READWRITE
        };

        self.do_map(protect)
    }

    pub fn map_exec_mut(self) -> Result<Mmap, Error> {
        if !self.unsafe_flags.contains(UnsafeMmapFlags::JIT) {
            return Err(Error::UnsafeFlagNeeded(UnsafeMmapFlags::JIT));
        }

        let protect = if self.flags.contains(MmapFlags::COPY_ON_WRITE) {
            PAGE_EXECUTE_WRITECOPY
        } else {
            PAGE_EXECUTE_READWRITE
        };

        self.do_map(protect)
    }
}
