//! Bounded reply waits without Vita's pthread absolute timeout clock.
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::Duration,
};

pub fn recv_timeout<T>(receiver: &Receiver<T>, timeout: Duration) -> Result<T, RecvTimeoutError> {
    #[cfg(target_os = "vita")]
    {
        relative_receive(receiver, timeout)
    }
    #[cfg(not(target_os = "vita"))]
    {
        receiver.recv_timeout(timeout)
    }
}

#[cfg(any(target_os = "vita", test))]
fn relative_receive<T>(receiver: &Receiver<T>, timeout: Duration) -> Result<T, RecvTimeoutError> {
    use std::{sync::mpsc::TryRecvError, time::Instant};
    let started = Instant::now();
    loop {
        match receiver.try_recv() {
            Ok(value) => return Ok(value),
            Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
            Err(TryRecvError::Empty) => {}
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(RecvTimeoutError::Timeout);
        }
        // Replies are infrequent; bound cancellation latency without timed park.
        std::thread::sleep(remaining.min(Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Instant};

    #[test]
    fn relative_reply_wait_preserves_delivery_deadline_and_disconnect() {
        let (tx, rx) = mpsc::channel();
        assert_eq!(
            relative_receive(&rx, Duration::ZERO),
            Err(RecvTimeoutError::Timeout)
        );
        tx.send(7).unwrap();
        assert_eq!(relative_receive(&rx, Duration::ZERO), Ok(7));
        let timeout = Duration::from_millis(3);
        let started = Instant::now();
        assert_eq!(
            relative_receive(&rx, timeout),
            Err(RecvTimeoutError::Timeout)
        );
        assert!(started.elapsed() >= timeout);
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            tx.send(9).unwrap();
        });
        assert_eq!(relative_receive(&rx, Duration::from_secs(2)), Ok(9));
        sender.join().unwrap();
        assert_eq!(
            relative_receive(&rx, Duration::from_secs(2)),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}
