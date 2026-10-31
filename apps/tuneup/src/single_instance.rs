//! Ensures only one TuneUp GUI process runs at a time.
//!
//! The primary instance binds a localhost TCP port and listens for a wake
//! command. A second launch connects, asks the primary to show the main window
//! (the primary acknowledges with `TUNEUP-OK`), then exits without starting
//! another UI.
//!
//! If the port is held by a *foreign* application, TuneUp logs a warning and
//! starts anyway (without the single-instance guard) instead of silently
//! refusing to launch.

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
const ACK: &[u8] = b"TUNEUP-OK\n";

/// Slot filled with `egui::Context` once the UI starts (for immediate wake).
pub type ContextSlot = Arc<Mutex<Option<egui::Context>>>;

/// Outcome of a single-instance check.
pub enum Started {
    /// We are the primary instance; the guard keeps the listener alive.
    Primary(InstanceGuard),
    /// Another TuneUp answered and was asked to show its window; exit now.
    Secondary,
}

/// Reasons the single-instance guard could not be established.
#[derive(Debug)]
pub enum StartError {
    /// The port is busy but the owner did not answer like TuneUp.
    ForeignPortOwner(String),
    /// The listener thread could not be started.
    ListenerFailed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignPortOwner(detail) => {
                write!(f, "порт {PORT} занят другим приложением: {detail}")
            }
            Self::ListenerFailed(detail) => write!(f, "не запущен слушатель: {detail}"),
        }
    }
}

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
/// # Errors
///
/// - [`StartError::ForeignPortOwner`] — the port is taken by something that
///   does not speak the TuneUp wake protocol. The caller should continue
///   starting the app without the guard.
///
/// # Example
///
/// ```ignore
/// let slot = context_slot();
/// let (tx, rx) = std::sync::mpsc::channel();
/// match ensure_single(tx.clone(), slot.clone()) {
///     Ok(Started::Primary(_guard)) => { /* run the UI */ }
///     Ok(Started::Secondary) => return, // primary shows its window
///     Err(error) => { /* log and run the UI without the guard */ }
/// }
/// ```
pub fn ensure_single(
    show_tx: Sender<TrayAction>,
    context_slot: ContextSlot,
) -> Result<Started, StartError> {
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
                                if let Ok(n) = stream.read(&mut buf)
                                    && buf[..n].starts_with(MAGIC)
                                {
                                    // Acknowledge so the second launch can
                                    // tell a real TuneUp from a foreign
                                    // port owner.
                                    let _ = stream.write_all(ACK);
                                    let _ = stream.flush();
                                    tracing::info!("second launch detected; showing main window");
                                    let _ = show_tx.send(TrayAction::OpenMainWindow);
                                    if let Ok(guard) = context_slot.lock()
                                        && let Some(context) = guard.as_ref()
                                    {
                                        context.request_repaint();
                                    }
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
                .map_err(|error| StartError::ListenerFailed(error.to_string()))?;
            tracing::info!(port = PORT, "single-instance primary ready");
            Ok(Started::Primary(InstanceGuard {
                stop,
                join: Some(join),
            }))
        }
        Err(error) => match notify_primary_show() {
            // A real TuneUp answered — it will show its window; we exit.
            true => Ok(Started::Secondary),
            // The port owner is not TuneUp. Starting anyway beats refusing
            // to launch; the only cost is the single-instance guard.
            false => Err(StartError::ForeignPortOwner(error.to_string())),
        },
    }
}

/// Sends the wake command and waits for the TuneUp acknowledgement.
fn notify_primary_show() -> bool {
    match TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], PORT)),
        Duration::from_millis(500),
    ) {
        Ok(mut stream) => {
            if let Err(error) = stream.write_all(MAGIC) {
                tracing::warn!(%error, "failed to send show request to primary");
                return false;
            }
            let _ = stream.flush();
            let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
            let mut buf = [0_u8; 16];
            match stream.read(&mut buf) {
                Ok(n) if buf[..n].starts_with(ACK) => true,
                Ok(_) => {
                    tracing::warn!("port owner answered, but is not TuneUp");
                    false
                }
                Err(error) => {
                    tracing::warn!(%error, "port owner did not answer like TuneUp");
                    false
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "could not reach primary TuneUp instance");
            false
        }
    }
}
