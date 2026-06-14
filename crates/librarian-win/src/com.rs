//! The dedicated COM single-threaded-apartment (STA) worker.
//!
//! Shell COM objects (`IFileOperation`, image factories, context menus) must be
//! created and called on a thread that initialized COM as an STA, and many
//! aren't safe to move between threads. Rather than scatter `CoInitializeEx`
//! calls around, we own exactly one STA thread and funnel every shell operation
//! to it as a closure. The closure runs on the STA thread; only its plain-data
//! result (bytes, `Vec`s, paths) crosses back, so the non-`Send` COM pointers
//! never leave the apartment.

use std::marker::PhantomData;
use std::sync::mpsc::{self, Sender};
use std::thread;

use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Proof that the current code is running on the COM STA worker thread inside an
/// initialized apartment.
///
/// Shell functions with apartment affinity ([`crate::delete_to_recycle`],
/// [`crate::icon_for_path`], `IFileOperation`, …) take an `&Apartment`. Only
/// [`ShellWorker::run`] hands one out, and it only ever runs its closure on the
/// STA thread — so "called on the correct apartment thread" becomes a
/// compile-time requirement the borrow checker enforces, rather than a
/// convention each call site has to remember.
pub struct Apartment {
    // A raw-pointer marker makes the witness `!Send + !Sync`, so it can't be
    // smuggled to another thread; the private field blocks construction anywhere
    // but this module (i.e. anywhere but the worker loop below).
    _not_send: PhantomData<*const ()>,
}

impl Apartment {
    fn new() -> Self {
        Self {
            _not_send: PhantomData,
        }
    }
}

/// Handle to the COM STA worker thread. Cheap to clone; all clones target the
/// same thread.
#[derive(Clone)]
pub struct ShellWorker {
    tx: Sender<Job>,
}

impl ShellWorker {
    /// Spawn the STA worker thread. Call once at startup and share clones.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();

        thread::Builder::new()
            .name("librarian-com".into())
            .spawn(move || {
                // SAFETY: paired with CoUninitialize when the loop ends. STA is
                // required for shell objects with UI affinity.
                let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                // S_FALSE (already initialized) still counts as success. A hard
                // failure here would make every shell call on this thread unsound,
                // so surface it loudly in debug builds.
                debug_assert!(
                    hr.is_ok(),
                    "CoInitializeEx failed on the COM worker thread: {hr:?}"
                );

                // Drains until every `ShellWorker` handle is dropped.
                while let Ok(job) = rx.recv() {
                    job();
                }

                unsafe { CoUninitialize() };
            })
            .expect("failed to spawn COM worker thread");

        Self { tx }
    }

    /// Run `f` on the COM STA thread and block until it returns its result.
    ///
    /// `f` receives an [`Apartment`] witness, which the apartment-affine shell
    /// wrappers require — so they can only be called from inside here.
    ///
    /// Intended to be called from a blocking-friendly context (an Iced
    /// `Task`/worker), never directly on the UI thread.
    pub fn run<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&Apartment) -> T + Send + 'static,
        T: Send + 'static,
    {
        let (rtx, rrx) = mpsc::channel();
        let job: Job = Box::new(move || {
            // The job body runs on the STA thread after CoInitializeEx, so
            // minting a witness here is sound.
            let apartment = Apartment::new();
            // Ignore send errors: only happens if the caller stopped waiting.
            let _ = rtx.send(f(&apartment));
        });
        self.tx.send(job).expect("COM worker thread is gone");
        rrx.recv().expect("COM worker dropped the job without replying")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_closures_on_the_worker_and_returns_results() {
        let worker = ShellWorker::spawn();
        assert_eq!(worker.run(|_| 2 + 2), 4);

        // The worker thread is distinct from the caller.
        let caller = thread::current().id();
        let worker_thread = worker.run(|_| thread::current());
        assert_ne!(caller, worker_thread.id());

        // A clone targets the same thread.
        let clone = worker.clone();
        let clone_thread = clone.run(|_| thread::current());
        assert_eq!(worker_thread.id(), clone_thread.id());
    }
}
