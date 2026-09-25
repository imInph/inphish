//! Read-only access to one table file. On Unix the file is memory mapped, so only the
//! pages a probe touches are read; elsewhere it is read into memory on first use.

use std::path::Path;

pub struct TableFile {
    #[cfg(unix)]
    mapping: unix::Mapping,
    #[cfg(not(unix))]
    bytes: Vec<u8>,
}

impl TableFile {
    pub fn open(path: &Path) -> Option<Self> {
        #[cfg(unix)]
        {
            unix::Mapping::open(path).map(|mapping| Self { mapping })
        }
        #[cfg(not(unix))]
        {
            std::fs::read(path).ok().map(|bytes| Self { bytes })
        }
    }

    pub fn bytes(&self) -> &[u8] {
        #[cfg(unix)]
        {
            self.mapping.bytes()
        }
        #[cfg(not(unix))]
        {
            &self.bytes
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    const PROT_READ: i32 = 1;
    const MAP_SHARED: i32 = 1;

    extern "C" {
        fn mmap(
            address: *mut c_void,
            length: usize,
            protection: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(address: *mut c_void, length: usize) -> i32;
    }

    pub struct Mapping {
        address: *mut c_void,
        length: usize,
    }

    // SAFETY: the mapping is read-only and never changes after creation.
    unsafe impl Send for Mapping {}
    // SAFETY: as above; shared references only read.
    unsafe impl Sync for Mapping {}

    impl Mapping {
        pub fn open(path: &Path) -> Option<Self> {
            let file = File::open(path).ok()?;
            let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
            if length == 0 {
                return None;
            }
            // SAFETY: a fresh read-only shared mapping of an open file descriptor; the
            // descriptor may be closed once the mapping exists.
            let address = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    length,
                    PROT_READ,
                    MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if address as isize == -1 || address.is_null() {
                return None;
            }
            Some(Self { address, length })
        }

        pub fn bytes(&self) -> &[u8] {
            // SAFETY: the mapping covers `length` readable bytes for its whole lifetime.
            unsafe { std::slice::from_raw_parts(self.address.cast::<u8>(), self.length) }
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: the address and length come from a successful mmap.
            unsafe {
                munmap(self.address, self.length);
            }
        }
    }
}
