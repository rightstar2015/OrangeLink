//! COM apartments for the threads OrangeLink starts that need one (the Media
//! Foundation engine, the PDF renderer).

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// This thread's place in the multithreaded apartment, left when dropped.
///
/// Declare it before every COM or WinRT object of the thread, so it is dropped
/// after them. A thread already in another apartment (the thumbnail workers are
/// STA) keeps it, and is not uninitialized here.
pub struct Apartment {
    joined: bool,
    /// Not Send: it must be dropped on the thread that joined.
    _thread: std::marker::PhantomData<*const ()>,
}

impl Apartment {
    pub fn mta() -> Self {
        // SAFETY: no pointers; paired with CoUninitialize in drop when it succeeded
        // (S_OK or S_FALSE).
        #[allow(unsafe_code)]
        let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self { joined, _thread: std::marker::PhantomData }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.joined {
            // SAFETY: pairs with the successful CoInitializeEx on this same thread
            // (the type is not Send).
            #[allow(unsafe_code)]
            unsafe {
                CoUninitialize()
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_leaves_only_an_apartment_it_joined() {
        std::thread::spawn(|| {
            let first = Apartment::mta();
            let again = Apartment::mta();
            assert!(first.joined && again.joined, "each join is paired with a leave");
        })
        .join()
        .unwrap();
        std::thread::spawn(|| {
            use windows::Win32::System::Com::COINIT_APARTMENTTHREADED;
            // SAFETY: no pointers; this test thread's own apartment.
            #[allow(unsafe_code)]
            let sta = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            assert!(sta.is_ok());
            assert!(!Apartment::mta().joined, "an STA thread keeps its apartment and isn't left");
            // SAFETY: pairs with the CoInitializeEx above.
            #[allow(unsafe_code)]
            unsafe {
                CoUninitialize()
            };
        })
        .join()
        .unwrap();
    }
}
