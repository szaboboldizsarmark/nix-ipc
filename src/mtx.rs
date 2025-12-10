use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    num::NonZeroUsize,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::io::AsRawFd,
    },
};

use anyhow::{Result, anyhow};
use nix::{
    errno::Errno,
    fcntl::{Flock, FlockArg, OFlag, open},
    libc::{
        EOWNERDEAD, PTHREAD_MUTEX_ROBUST, PTHREAD_PROCESS_SHARED, c_int, dup, munmap, off_t,
        pthread_mutex_consistent, pthread_mutex_init, pthread_mutex_lock, pthread_mutex_t,
        pthread_mutex_unlock, pthread_mutexattr_destroy, pthread_mutexattr_init,
        pthread_mutexattr_setpshared, pthread_mutexattr_setrobust, pthread_mutexattr_t,
    },
    sys::{
        mman::{MapFlags, ProtFlags, mmap},
        stat::Mode,
    },
    unistd::ftruncate,
};

/// The result of locking an interprocess mutex.
#[derive(Debug, Clone)]
pub enum LockResult {
    /// Mutex acquired normally without prior owner death.
    Acquired,
    /// Mutex acquired after recovering from a previous owner's death.
    OwnerDiedRecovered,
}

pub struct Mtx {
    _fd: OwnedFd,
    ptr: *mut pthread_mutex_t,
}

impl Mtx {
    pub fn new(name: &str) -> Result<Self> {
        let path = format!("/dev/shm/{}.mtx", name);
        let fd = open(
            path.as_str(),
            OFlag::O_CREAT | OFlag::O_RDWR,
            Mode::from_bits_truncate(0o600),
        )?;

        ftruncate(&fd, size_of::<pthread_mutex_t>() as off_t)?;

        let dup_raw_fd = unsafe { Errno::result(dup(fd.as_raw_fd()))? };
        let dup_fd = unsafe { OwnedFd::from_raw_fd(dup_raw_fd) };

        let init_lock = Flock::lock(dup_fd, FlockArg::LockExclusive)
            .map_err(|(_, e)| anyhow!("init-lock failed: {}", e))?;

        let len = NonZeroUsize::new(size_of::<pthread_mutex_t>())
            .expect("pthread_mutex_t has nonzero size");
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

        let mtx_ptr = raw_ptr.as_ptr() as *mut pthread_mutex_t;

        let first = unsafe { *(mtx_ptr as *const c_int) };
        if first == 0 {
            let mut attr: pthread_mutexattr_t = unsafe { zeroed() };
            unsafe {
                Errno::result(pthread_mutexattr_init(&mut attr))?;
                Errno::result(pthread_mutexattr_setpshared(
                    &mut attr,
                    PTHREAD_PROCESS_SHARED,
                ))?;
                Errno::result(pthread_mutexattr_setrobust(&mut attr, PTHREAD_MUTEX_ROBUST))?;
                Errno::result(pthread_mutex_init(mtx_ptr, &attr))?;
                Errno::result(pthread_mutexattr_destroy(&mut attr))?;
            }
        }

        init_lock
            .unlock()
            .map_err(|(_, e)| anyhow!("init-unlock failed: {}", e))?;

        Ok(Self {
            _fd: fd,
            ptr: mtx_ptr,
        })
    }

    pub fn lock(&self) -> Result<LockResult> {
        let err = unsafe { pthread_mutex_lock(self.ptr) };
        if err == EOWNERDEAD {
            unsafe {
                Errno::result(pthread_mutex_consistent(self.ptr))
                    .map_err(|e| anyhow!("pthread_mutex_consistent failed: {e}"))?;
            }
            Ok(LockResult::OwnerDiedRecovered)
        } else {
            Errno::result(err)
                .map(|_| LockResult::Acquired)
                .map_err(|e| anyhow!("pthread_mutex_lock failed: {e}"))
        }
    }

    pub fn unlock(&self) -> Result<()> {
        unsafe {
            Errno::result(pthread_mutex_unlock(self.ptr))
                .map(|_| ())
                .map_err(|e| anyhow!("pthread_mutex_unlock failed: {e}"))
        }
    }
}

impl Drop for Mtx {
    fn drop(&mut self) {
        unsafe {
            // Don't destroy the on-disk mutex so it remains valid for other processes
            // Errno::result(nix::libc::pthread_mutex_destroy(self.ptr)).ok();
            Errno::result(munmap(
                self.ptr as *mut c_void,
                size_of::<pthread_mutex_t>(),
            ))
            .ok();
        }
    }
}
