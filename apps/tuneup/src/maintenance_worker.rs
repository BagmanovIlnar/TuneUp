//! Background worker for cleanup and uninstall operations.
//!
//! Keeps filesystem and package-manager work off the egui UI thread.

use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

use eframe::egui;
use tuneup_core::{
    cleanup::{CleanupReport, CleanupScan, CleanupSelection},
    uninstall::{InstalledApplication, LeftoverItem, UninstallReport, UninstallRequest},
};
use tuneup_platform::{CleanupProvider, UninstallProvider};

enum Command {
    ScanCleanup,
    Clean(CleanupSelection),
    LoadApplications,
    ScanLeftovers(String),
    Uninstall(UninstallRequest),
    Stop,
}

/// Events produced by [`MaintenanceWorker`].
#[derive(Debug)]
pub enum MaintenanceEvent {
    CleanupScan(Result<CleanupScan, String>),
    CleanupDone(Result<CleanupReport, String>),
    Applications(Result<Vec<InstalledApplication>, String>),
    Leftovers {
        application_id: String,
        result: Result<Vec<LeftoverItem>, String>,
    },
    UninstallDone(Result<UninstallReport, String>),
}

/// Asynchronous maintenance orchestrator used by the GUI.
pub struct MaintenanceWorker {
    command_tx: Sender<Command>,
    event_rx: Receiver<MaintenanceEvent>,
    worker: Option<JoinHandle<()>>,
}

impl MaintenanceWorker {
    /// Starts a background thread bound to the given providers.
    pub fn start(
        repaint: egui::Context,
        cleanup: Arc<dyn CleanupProvider>,
        uninstaller: Arc<dyn UninstallProvider>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                let event = match command {
                    Command::Stop => break,
                    Command::ScanCleanup => MaintenanceEvent::CleanupScan(
                        cleanup.scan().map_err(|error| error.to_string()),
                    ),
                    Command::Clean(selection) => MaintenanceEvent::CleanupDone(
                        cleanup.clean(&selection).map_err(|error| error.to_string()),
                    ),
                    Command::LoadApplications => MaintenanceEvent::Applications(
                        uninstaller
                            .installed_applications()
                            .map_err(|error| error.to_string()),
                    ),
                    Command::ScanLeftovers(application_id) => {
                        let result = uninstaller
                            .find_leftovers(&application_id)
                            .map_err(|error| error.to_string());
                        MaintenanceEvent::Leftovers {
                            application_id,
                            result,
                        }
                    }
                    Command::Uninstall(request) => MaintenanceEvent::UninstallDone(
                        uninstaller
                            .uninstall(&request)
                            .map_err(|error| error.to_string()),
                    ),
                };
                let _ = event_tx.send(event);
                repaint.request_repaint();
            }
        });
        Self {
            command_tx,
            event_rx,
            worker: Some(worker),
        }
    }

    /// Requests a cleanup scan.
    pub fn scan_cleanup(&self) {
        let _ = self.command_tx.send(Command::ScanCleanup);
    }

    /// Requests cleanup of the selected identifiers.
    pub fn clean(&self, selection: CleanupSelection) {
        let _ = self.command_tx.send(Command::Clean(selection));
    }

    /// Requests the installed application list.
    pub fn load_applications(&self) {
        let _ = self.command_tx.send(Command::LoadApplications);
    }

    /// Requests leftover discovery for one application.
    pub fn scan_leftovers(&self, application_id: String) {
        let _ = self.command_tx.send(Command::ScanLeftovers(application_id));
    }

    /// Requests uninstall plus leftover cleanup.
    pub fn uninstall(&self, request: UninstallRequest) {
        let _ = self.command_tx.send(Command::Uninstall(request));
    }

    /// Drains the latest events, keeping only the newest of each kind when many arrive.
    pub fn take_events(&self) -> Vec<MaintenanceEvent> {
        self.event_rx.try_iter().collect()
    }
}

impl Drop for MaintenanceWorker {
    fn drop(&mut self) {
        let _ = self.command_tx.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
