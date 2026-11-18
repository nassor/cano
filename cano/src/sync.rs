//! Internal synchronization primitives unifying `parking_lot` (native
//! `runtime-tokio`) and `std::sync` (WebAssembly / WASI Preview 3).

#[cfg(feature = "runtime-tokio")]
pub(crate) use parking_lot::{Mutex, RwLock};

#[cfg(not(feature = "runtime-tokio"))]
mod std_sync {
    use std::sync::{MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard};

    /// Non-poisoning wrapper around [`std::sync::Mutex`] matching
    /// `parking_lot::Mutex`'s infallible `lock()` API.
    #[repr(transparent)]
    #[derive(Default, Debug)]
    pub(crate) struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

    impl<T> Mutex<T> {
        #[inline]
        pub(crate) fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }
    }

    impl<T: ?Sized> Mutex<T> {
        #[inline]
        pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    /// Non-poisoning wrapper around [`std::sync::RwLock`] matching
    /// `parking_lot::RwLock`'s infallible `read()` / `write()` API.
    #[repr(transparent)]
    #[derive(Default, Debug)]
    pub(crate) struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

    impl<T> RwLock<T> {
        #[inline]
        pub(crate) fn new(value: T) -> Self {
            Self(std::sync::RwLock::new(value))
        }
    }

    impl<T: ?Sized> RwLock<T> {
        #[inline]
        pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
            self.0.read().unwrap_or_else(PoisonError::into_inner)
        }

        #[inline]
        pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
            self.0.write().unwrap_or_else(PoisonError::into_inner)
        }
    }
}

#[cfg(not(feature = "runtime-tokio"))]
pub(crate) use std_sync::{Mutex, RwLock};
