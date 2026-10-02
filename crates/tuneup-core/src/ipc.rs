use std::path::PathBuf;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    cleanup::CleanupReport, error::IpcError, model::PlatformBackup, uninstall::UninstallReport,
};

pub const IPC_PROTOCOL_VERSION: u32 = 3;
pub const PIPE_NAME: &str = r"\\.\pipe\TuneUp.Elevated.v3";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub version: u32,
    pub nonce: u64,
    pub session_mac: [u8; 32],
    pub command: HelperCommand,
}

impl IpcRequest {
    pub fn signed(
        nonce: u64,
        command: HelperCommand,
        session_key: &[u8; 32],
    ) -> Result<Self, IpcError> {
        let session_mac = calculate_mac(nonce, &command, session_key)?;
        Ok(Self {
            version: IPC_PROTOCOL_VERSION,
            nonce,
            session_mac,
            command,
        })
    }

    pub fn verify(&self, session_key: &[u8; 32]) -> Result<(), IpcError> {
        if self.version != IPC_PROTOCOL_VERSION {
            return Err(IpcError::VersionMismatch(self.version));
        }
        let bytes = signing_bytes(self.nonce, &self.command)?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(session_key).map_err(|_| IpcError::AuthFailed)?;
        mac.update(&bytes);
        mac.verify_slice(&self.session_mac)
            .map_err(|_| IpcError::AuthFailed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub version: u32,
    pub nonce: u64,
    pub result: Result<HelperResponse, HelperError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HelperCommand {
    Ping,
    GetCapabilities,
    ApplyPlatform(DisablePlatformRequest),
    RestorePlatform(PlatformBackup),
    /// Deletes previously validated cleanup paths under allowlisted roots.
    CleanPaths(CleanPathsRequest),
    /// Runs a fixed uninstall operation identified by package/product id.
    UninstallPackage(UninstallPackageRequest),
}

/// Fixed cleanup request accepted by the elevated helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanPathsRequest {
    /// Absolute paths that the helper will re-validate against an allowlist.
    pub paths: Vec<PathBuf>,
}

/// Fixed package uninstall request accepted by the elevated helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallPackageRequest {
    /// Opaque package / product identifier already validated by the GUI provider.
    pub identifier: String,
    /// Machine-readable kind: `msi`, `deb`, `rpm`, `flatpak`, `snap`.
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisablePlatformRequest {
    pub install_root: PathBuf,
    #[serde(default)]
    pub include_hklm_run: bool,
    #[serde(default)]
    pub include_services: bool,
    #[serde(default)]
    pub include_tasks: bool,
    #[serde(default)]
    pub include_common_startup: bool,
    #[serde(default)]
    pub include_macos_system: bool,
    #[serde(default)]
    pub include_linux_systemd: bool,
    #[serde(default)]
    pub include_linux_xdg: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HelperResponse {
    Pong {
        elevated: bool,
        build: String,
    },
    Capabilities(CapabilityFlags),
    PlatformApplied {
        backup: PlatformBackup,
        errors: Vec<String>,
    },
    PlatformRestored {
        remaining: PlatformBackup,
    },
    Cleanup(CleanupReport),
    Uninstall(UninstallReport),
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CapabilityFlags {
    #[serde(default)]
    pub hklm_run: bool,
    #[serde(default)]
    pub services: bool,
    #[serde(default)]
    pub tasks: bool,
    #[serde(default)]
    pub common_startup: bool,
    #[serde(default)]
    pub macos_launchd: bool,
    #[serde(default)]
    pub macos_login_items: bool,
    #[serde(default)]
    pub linux_systemd: bool,
    #[serde(default)]
    pub linux_xdg: bool,
    #[serde(default)]
    pub cleanup: bool,
    #[serde(default)]
    pub uninstall: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperError {
    pub code: HelperErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum HelperErrorCode {
    AuthFailed,
    InvalidRequest,
    AccessDenied,
    Internal,
}

fn calculate_mac(
    nonce: u64,
    command: &HelperCommand,
    session_key: &[u8; 32],
) -> Result<[u8; 32], IpcError> {
    let bytes = signing_bytes(nonce, command)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(session_key).map_err(|_| IpcError::AuthFailed)?;
    mac.update(&bytes);
    Ok(mac.finalize().into_bytes().into())
}

fn signing_bytes(nonce: u64, command: &HelperCommand) -> Result<Vec<u8>, IpcError> {
    serde_json::to_vec(&(IPC_PROTOCOL_VERSION, nonce, command))
        .map_err(|error| IpcError::Serialize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_request_rejects_modified_nonce() {
        let key = [7; 32];
        let mut request = IpcRequest::signed(1, HelperCommand::Ping, &key).unwrap();
        request.verify(&key).unwrap();
        request.nonce = 2;
        assert!(matches!(request.verify(&key), Err(IpcError::AuthFailed)));
    }

    #[test]
    fn protocol_round_trip() {
        let request = IpcRequest::signed(10, HelperCommand::GetCapabilities, &[1; 32]).unwrap();
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: IpcRequest = serde_json::from_slice(&encoded).unwrap();
        decoded.verify(&[1; 32]).unwrap();
    }
}
