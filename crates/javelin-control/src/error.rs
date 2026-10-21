use std::{error::Error, fmt};

use javelin_protocol::{ProtocolError, codec::code};

/// A control-plane refusal. [`ControlError::code`] gives the protocol code where one exists.
#[derive(Debug)]
pub enum ControlError {
    NotInitialized,
    AlreadyInitialized,
    /// The caller is not authenticated.
    Unauthorized(&'static str),
    /// The caller is authenticated but may not do this.
    Forbidden(&'static str),
    InvalidInput(String),
    NotFound(&'static str),
    /// A device code or its approval is past its lifetime.
    Expired(&'static str),
    /// A device code was already exchanged for a session.
    Redeemed,
    NameTaken,
    /// The operation ID was used for a different request.
    OperationReused,
    /// The node could not be provisioned or reached.
    Node(String),
    /// The node refused a permissions push.
    Refused(ProtocolError),
    Storage(String),
}

impl ControlError {
    pub fn code(&self) -> &str {
        match self {
            Self::NotInitialized => "not_initialized",
            Self::AlreadyInitialized | Self::NameTaken => code::CONFLICT,
            Self::Unauthorized(_) => code::UNAUTHORIZED,
            Self::Forbidden(_) => code::FORBIDDEN,
            Self::InvalidInput(_) => code::INVALID_INPUT,
            Self::NotFound(_) => "not_found",
            Self::Expired(_) => "expired",
            Self::Redeemed => "already_redeemed",
            Self::OperationReused => code::OPERATION_ID_REUSED,
            Self::Node(_) => code::NODE_UNAVAILABLE,
            Self::Refused(error) => &error.code,
            Self::Storage(_) => "storage_error",
        }
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => f.write_str(
                "the control plane is not initialized; run `javelin control init` first",
            ),
            Self::AlreadyInitialized => f.write_str(
                "the control plane is already initialized; its admin token was shown once at init",
            ),
            Self::Unauthorized(reason) | Self::Forbidden(reason) | Self::NotFound(reason) => {
                f.write_str(reason)
            }
            Self::Expired(reason) => f.write_str(reason),
            Self::InvalidInput(reason) => f.write_str(reason),
            Self::Redeemed => f.write_str("this device code was already used to sign in"),
            Self::NameTaken => f.write_str("a repository with this name already exists"),
            Self::OperationReused => {
                f.write_str("this operation ID was already used for a different request")
            }
            Self::Node(detail) => write!(f, "node unavailable: {detail}"),
            Self::Refused(error) => write!(f, "node refused: {}: {}", error.code, error.detail),
            Self::Storage(detail) => write!(f, "control state: {detail}"),
        }
    }
}

impl Error for ControlError {}
