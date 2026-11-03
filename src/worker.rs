//! Background work with cancellation (spec §5.2 `worker`, §9).
//!
//! One place for starting a thread, handing back its result, and cancelling it,
//! so diffing and directory scanning do not each wire this up separately.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// A shared "please stop" flag. Cloning shares the flag.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> CancelFlag {
        CancelFlag::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    /// For code that takes a plain `AtomicBool`.
    pub fn as_atomic(&self) -> &AtomicBool {
        &self.0
    }
}

/// What a job has produced so far.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll<T> {
    Ready(T),
    Pending,
    /// The job ended without a result (it panicked or was dropped).
    Lost,
}

/// A computation running on its own thread that yields one result.
///
/// Dropping or cancelling a job abandons it: its result is thrown away. A
/// running computation is only interrupted if it looks at the [`CancelFlag`]
/// itself (a directory scan does; the line diff cannot), otherwise it finishes
/// in the background and is ignored, or ends with the process.
pub struct Job<T> {
    rx: Receiver<T>,
    cancel: CancelFlag,
    started: Instant,
}

impl<T: Send + 'static> Job<T> {
    pub fn spawn(f: impl FnOnce(&CancelFlag) -> T + Send + 'static) -> Job<T> {
        let (tx, rx) = mpsc::channel();
        let cancel = CancelFlag::new();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let result = f(&flag);
            // The receiver is gone if the job was abandoned; nothing to do.
            let _ = tx.send(result);
        });
        Job {
            rx,
            cancel,
            started: Instant::now(),
        }
    }

    /// A job whose result is supplied by the caller through the returned sender.
    /// For tests and for hosts that compute the result themselves.
    pub fn manual() -> (Job<T>, Sender<T>) {
        let (tx, rx) = mpsc::channel();
        (
            Job {
                rx,
                cancel: CancelFlag::new(),
                started: Instant::now(),
            },
            tx,
        )
    }

    pub fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(v) => Poll::Ready(v),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }

    /// Wait up to `limit` for the result.
    pub fn wait(&self, limit: Duration) -> Poll<T> {
        match self.rx.recv_timeout(limit) {
            Ok(v) => Poll::Ready(v),
            Err(RecvTimeoutError::Timeout) => Poll::Pending,
            Err(RecvTimeoutError::Disconnected) => Poll::Lost,
        }
    }

    /// Block until the job finishes.
    pub fn wait_forever(&self) -> Poll<T> {
        match self.rx.recv() {
            Ok(v) => Poll::Ready(v),
            Err(_) => Poll::Lost,
        }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Run `f` on a thread that reports many messages (a directory scan). Returns
/// the receiving end and the flag that asks it to stop. Dropping the receiver
/// also ends the work, because sends then fail.
pub fn spawn_stream<M: Send + 'static>(
    f: impl FnOnce(&CancelFlag, &mut dyn FnMut(M)) + Send + 'static,
) -> (Receiver<M>, CancelFlag) {
    let (tx, rx) = mpsc::channel();
    let cancel = CancelFlag::new();
    let flag = cancel.clone();
    std::thread::spawn(move || {
        f(&flag, &mut |m| {
            let _ = tx.send(m);
        });
    });
    (rx, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_delivers_its_result() {
        let job = Job::spawn(|_| 6 * 7);
        assert_eq!(job.wait(Duration::from_secs(5)), Poll::Ready(42));
    }

    #[test]
    fn polling_a_slow_job_is_pending_until_it_finishes() {
        let (job, tx) = Job::<u8>::manual();
        assert_eq!(job.poll(), Poll::Pending);
        assert_eq!(job.wait(Duration::from_millis(5)), Poll::Pending);
        tx.send(7).unwrap();
        assert_eq!(job.wait_forever(), Poll::Ready(7));
    }

    #[test]
    fn dropping_a_job_cancels_it_and_the_worker_sees_the_flag() {
        let (seen_tx, seen_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let job = Job::spawn(move |c| {
            go_rx.recv().unwrap();
            seen_tx.send(c.is_cancelled()).unwrap();
        });
        drop(job);
        go_tx.send(()).unwrap();
        assert!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn a_panicking_job_is_reported_as_lost() {
        let job = Job::<u8>::spawn(|_| panic!("boom"));
        assert_eq!(job.wait_forever(), Poll::Lost);
    }

    #[test]
    fn streams_deliver_in_order_and_stop_when_cancelled() {
        let (rx, cancel) = spawn_stream(|c, emit| {
            for i in 0..1_000_000u32 {
                if c.is_cancelled() {
                    break;
                }
                emit(i);
            }
        });
        assert_eq!(rx.recv().unwrap(), 0);
        assert_eq!(rx.recv().unwrap(), 1);
        cancel.cancel();
        let rest = rx.iter().count();
        assert!(rest < 1_000_000, "the producer stopped early");
    }
}
