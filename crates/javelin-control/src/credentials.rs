//! The device's sign-in, kept on this machine only. The refresh token is usable only
//! through the control plane, so it is never sent to a node.

use std::{fs, io::ErrorKind, path::Path};

use serde::{Deserialize, Serialize};

use crate::{ControlError, secret};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub principal: String,
    pub device: String,
    pub refresh_token: String,
}

impl Credentials {
    /// Reads the credentials at `path`. Missing credentials mean the device is not signed in.
    pub fn load(path: &Path) -> Result<Self, ControlError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(ControlError::Unauthorized(
                    "not signed in; run `javelin login` first",
                ));
            }
            Err(error) => return Err(ControlError::Storage(error.to_string())),
        };
        serde_json::from_slice(&bytes)
            .map_err(|error| ControlError::Storage(format!("credentials are corrupt: {error}")))
    }

    /// Writes the credentials to `path`, private to the owner.
    pub fn save(&self, path: &Path) -> Result<(), ControlError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| ControlError::Storage(error.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| ControlError::Storage(error.to_string()))?;
        secret::write_private(path, &bytes)
            .map_err(|error| ControlError::Storage(error.to_string()))
    }
}
