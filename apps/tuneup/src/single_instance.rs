//! Ensures only one TuneUp GUI process runs at a time.
//!
//! The primary instance binds a localhost TCP port and listens for a wake
//! command. A second launch connects, asks the primary to show the main window,
//! then exits without starting another UI.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::Duration,
};

use eframe::egui;

use crate::tray::TrayAction;

/// Localhost port reserved for TuneUp single-instance wake protocol.
const PORT: u16 = 47_821;
const MAGIC: &[u8] = b"TUNEUP-SHOW\n";

/// Slot filled with `egui::Context` once the UI starts (for immediate wake).
pub type ContextSlot = Arc<Mutex<Option<egui::Context>>>;

/// Holds the primary-instance listener thread until drop.
pub struct InstanceGuard {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], PORT)),
            Duration::from_millis(50),
        );
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Creates an empty context slot for the primary instance.
pub fn context_slot() -> ContextSlot {
    Arc::new(Mutex::new(None))
}

/// Becomes the primary instance, or notifies an already-running primary to show
/// its window.
///
/// # Example
///
/// ```ignore
/// let slot = context_slot();
/// let (tx, rx) = std::sync::mpsc::channel();
/// let Ok(_guard) = ensure_single(tx.clone(), slot.clone()) else {
///     return; // secondary: primary was asked to open the window
/// };
/// ```
pub fn ensure_single(show_tx: Sender<TrayAction>, context_slot: ContextSlot) -> Result<InstanceGuard, ()> {
    match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(listener) => {
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = Arc::clone(&stop);
            let join = thread::Builder::new()
                .name("tuneup-instance".into())
                .spawn(move || {
                    let _ = listener.set_nonblocking(true);
                    while !stop_thread.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                                let mut buf = [0_u8; 32];
                                match stream.read(&mut buf) {
                                    Ok(n) if buf[..n].starts_with(MAGIC) => {
                                        tracing::info!(
                                            "second launch detected; showing main window"
                                        );
                                        let _ = show_tx.send(TrayAction::OpenMainWindow);
                                        if let Ok(guard) = context_slot.lock() {
                                            if let Some(context) = guard.as_ref() {
                                                context.request_repaint();
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    || error.kind() == std::io::ErrorKind::TimedOut =>
                            {
                                thread::sleep(Duration::from_millis(100));
                            }
                            Err(_) => thread::sleep(Duration::from_millis(100)),
                        }
                    }
                })
                .map_err(|error| {
                    tracing::error!(%error, "failed to start single-instance listener");
                })?;
            tracing::info!(port = PORT, "single-instance primary ready");
            Ok(InstanceGuard {
                stop,
                join: Some(join),
            })
        }
        Err(error) => {
            tracing::info!(%error, "TuneUp already running; requesting show");
            notify_primary_show();
            Err(())
        }
    }
}

fn notify_primary_show() {
    match TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], PORT)),
        Duration::from_millis(500),
    ) {
        Ok(mut stream) => {
            if let Err(error) = stream.write_all(MAGIC) {
                tracing::warn!(%error, "failed to send show request to primary");
            } else {
                let _ = stream.flush();
            }
        }
        Err(error) => {
            tracing::warn!(%error, "could not reach primary TuneUp instance");
        }
    }
}
