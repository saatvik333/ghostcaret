//! Hand-off to the backend's threads, each of which waits in a Win32 message
//! loop: a bounded channel, plus one posted message to wake the thread that
//! is coalesced until the thread has drained its inbox.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_NOREMOVE,
    PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT, WM_APP, WM_QUIT,
};

/// Posted to a thread to make it drain its inbox.
const WM_MAIL: u32 = WM_APP + 1;
/// Messages waiting for a thread before senders are told it is not keeping up.
const CAPACITY: usize = 256;

#[derive(Debug)]
pub struct Mailbox<T> {
    tx: Sender<T>,
    thread: u32,
    woken: Arc<AtomicBool>,
}

impl<T> Clone for Mailbox<T> {
    fn clone(&self) -> Self {
        Mailbox {
            tx: self.tx.clone(),
            thread: self.thread,
            woken: self.woken.clone(),
        }
    }
}

impl<T> Mailbox<T> {
    /// Queues `msg` and wakes the thread. Returns false when the thread is
    /// gone or `CAPACITY` messages behind; the message is dropped.
    pub fn post(&self, msg: T) -> bool {
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                log::error!(
                    "thread {} is {CAPACITY} messages behind; dropping one",
                    self.thread
                );
                return false;
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
        if !self.woken.swap(true, Ordering::AcqRel) {
            // SAFETY: posting a parameterless message to a thread id is always sound;
            // it fails harmlessly if the thread has exited.
            let _ = unsafe { PostThreadMessageW(self.thread, WM_MAIL, WPARAM(0), LPARAM(0)) };
        }
        true
    }

    pub fn thread(&self) -> u32 {
        self.thread
    }
}

#[derive(Debug)]
pub struct Inbox<T> {
    rx: Receiver<T>,
    woken: Arc<AtomicBool>,
}

impl<T> Inbox<T> {
    /// Takes every queued message.
    pub fn drain(&self) -> impl Iterator<Item = T> + '_ {
        self.woken.store(false, Ordering::Release);
        self.rx.try_iter()
    }
}

/// Tells [`spawn`] that the thread has started, or why it could not.
#[derive(Debug)]
pub struct Ready(Sender<Result<u32, String>>);

impl Ready {
    pub fn ok(self) {
        // SAFETY: always callable.
        let _ = self.0.send(Ok(unsafe { GetCurrentThreadId() }));
    }

    pub fn fail(self, why: String) {
        let _ = self.0.send(Err(why));
    }
}

/// Starts a named thread running `body` and waits until it reports ready.
pub fn spawn<T: Send + 'static>(
    name: &str,
    body: impl FnOnce(Inbox<T>, Ready) + Send + 'static,
) -> Result<(Mailbox<T>, JoinHandle<()>), String> {
    spawn_with_self(name, |inbox, ready, _| body(inbox, ready))
}

/// Like [`spawn`], also handing the thread its own mailbox, for callbacks
/// that run elsewhere and post back to it.
pub fn spawn_with_self<T: Send + 'static>(
    name: &str,
    body: impl FnOnce(Inbox<T>, Ready, Mailbox<T>) + Send + 'static,
) -> Result<(Mailbox<T>, JoinHandle<()>), String> {
    let (tx, rx) = bounded(CAPACITY);
    let woken = Arc::new(AtomicBool::new(false));
    let inbox = Inbox {
        rx,
        woken: woken.clone(),
    };
    let own = (tx.clone(), woken.clone());
    let (ready_tx, ready_rx) = bounded(1);
    let handle = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // Create the message queue first, so anything posted to the
            // thread from now on (even by its own callbacks) arrives.
            let mut msg = MSG::default();
            // SAFETY: `msg` is a valid out-pointer; PM_NOREMOVE leaves the queue intact.
            let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
            // SAFETY: always callable.
            let thread = unsafe { GetCurrentThreadId() };
            body(
                inbox,
                Ready(ready_tx),
                Mailbox {
                    tx: own.0,
                    thread,
                    woken: own.1,
                },
            )
        })
        .map_err(|e| format!("cannot start the {name} thread: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(thread)) => Ok((Mailbox { tx, thread, woken }, handle)),
        Ok(Err(why)) => {
            let _ = handle.join();
            Err(why)
        }
        Err(_) => {
            let _ = handle.join();
            Err(format!("the {name} thread stopped while starting"))
        }
    }
}

/// Asks a thread's message loop to end.
pub fn quit(thread: u32) {
    // SAFETY: see `Mailbox::post`.
    let _ = unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
}

/// Runs the calling thread's message loop until `WM_QUIT`. Window messages
/// are dispatched, and sent messages, including low-level hook calls, are
/// delivered inside `PeekMessageW`. `mail` runs when the inbox has messages;
/// `idle` runs whenever the queue is empty and returns how long to wait
/// (milliseconds, `u32::MAX` for no limit) before calling it again.
pub fn run(mut mail: impl FnMut(), mut idle: impl FnMut() -> u32) {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer for every call.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            match msg.message {
                WM_QUIT => return,
                WM_MAIL if msg.hwnd.is_invalid() => mail(),
                // SAFETY: `msg` was just filled in by PeekMessageW.
                _ => unsafe {
                    DispatchMessageW(&msg);
                },
            }
        }
        let wait = idle();
        // Wakes for posted and sent messages alike (QS_ALLINPUT includes
        // QS_SENDMESSAGE, which low-level hook calls set), so this loop
        // serves hooks exactly as GetMessageW would.
        // SAFETY: no handles are passed; the flags are valid.
        let _ =
            unsafe { MsgWaitForMultipleObjectsEx(None, wait, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
    }
}
