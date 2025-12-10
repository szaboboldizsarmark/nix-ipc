// UNSAFECELL EXPLANATION:
//
// The `ptr` field is wrapped in an `UnsafeCell<T>` because it's pointing to shared memory.
// Shared memory can be changed from outside the Rust program, such as by another program or process.
//
// View should be a simple pointer pointing to the shared memory. Why is UnsafeCell necessary?
// - Normally, Rust assumes that data accessed through an immutable reference (`&T`) doesn't change (even when declared mut).
//   Because of this assumption, the compiler might apply optimizations that remove or reorder memory reads.
// - However, our shared memory can be modified externally, meaning Rust's assumptions don't hold.
//
// How does UnsafeCell help?
// - UnsafeCell explicitly tells Rust that the memory might change unexpectedly.
//   This prevents incorrect compiler optimizations like caching old values or skipping reads.
//
// In summary:
// UnsafeCell helps Rust correctly handle shared memory by preventing incorrect assumptions,
// and synchronization (like mutexes) ensures safe, correct access.

use std::{cell::UnsafeCell, ffi::c_void, mem::size_of, num::NonZeroUsize, os::fd::OwnedFd};

use anyhow::{Result, anyhow};
use nix::{
    errno::Errno,
    fcntl::{OFlag, open},
    libc::{munmap, off_t},
    sys::{
        mman::{MapFlags, ProtFlags, mmap},
        stat::Mode,
    },
    unistd::ftruncate,
};

pub struct Shm<T: 'static> {
    _fd: OwnedFd,
    ptr: *mut UnsafeCell<T>,
    len: NonZeroUsize,
}

impl<T: 'static> Shm<T> {
    /// Creates or opens a shared memory object in /dev/shm and maps it.
    pub fn new(name: &str) -> Result<Self> {
        let path = format!("/dev/shm/{}", name);
        let shm_size = size_of::<T>();

        let len = NonZeroUsize::new(shm_size)
            .ok_or_else(|| anyhow!("Cannot use zero-sized type in shared memory"))?;

        let fd = open(
            path.as_str(),
            OFlag::O_CREAT | OFlag::O_RDWR,
            Mode::from_bits_truncate(0o600),
        )?;

        ftruncate(&fd, shm_size as off_t)?;

        let raw_ptr = unsafe {
            mmap(
                None,
                len,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &fd,
                0,
            )?
        };

        let data_ptr = raw_ptr.as_ptr() as *mut UnsafeCell<T>;

        Ok(Self {
            _fd: fd,
            ptr: data_ptr,
            len,
        })
    }

    /// Provides exclusive access to the shared memory data using a closure.
    pub fn access<R, F>(&mut self, accessor: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let data = unsafe { &mut *self.ptr };
        accessor(data.get_mut())
    }

    // use nix::unistd::unlink;
    //
    // /// Unlinks (deletes) the shared memory object from the filesystem.
    // pub fn unlink(name: &str) -> Result<()> {
    //     let path = format!("/dev/shm/{}", name);
    //     println!("Attempting to unlink shared memory object: {}", path);
    //     unlink(path.as_str())?;
    //     Ok(())
    // }
}

impl<T: 'static> Drop for Shm<T> {
    fn drop(&mut self) {
        unsafe {
            std::ptr::drop_in_place(self.ptr);
            Errno::result(munmap(self.ptr as *mut c_void, self.len.get())).ok();
        }
    }
}
