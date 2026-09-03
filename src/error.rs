use serde::Serialize;
use serde_json::json;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub struct JavelinError {
    pub exit_code: u8,
    pub code: &'static str,
    pub message: String,
    pub details: serde_json::Value,
    pub recovery: Vec<String>,
}

impl JavelinError {
    pub fn new(exit_code: u8, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            code,
            message: message.into(),
            details: json!({}),
            recovery: Vec::new(),
        }
    }

    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    pub fn recovery(mut self, recovery: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.recovery = recovery.into_iter().map(Into::into).collect();
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(2, "INVALID_ARGUMENT", message)
    }

    pub fn no_world(path: impl Display) -> Self {
        Self::new(3, "NO_WORLD", format!("no Javelin World found from {path}"))
            .recovery(["javelin init"])
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(4, "CONFLICT", message)
    }

    pub fn verification(message: impl Into<String>) -> Self {
        Self::new(5, "VERIFICATION_FAILED", message)
    }

    pub fn stale(message: impl Into<String>) -> Self {
        Self::new(6, "STALE_STATE", message)
    }

    pub fn corruption(message: impl Into<String>) -> Self {
        Self::new(7, "STORAGE_CORRUPTION", message)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(8, "RESOURCE_BUSY", message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(9, "UNSUPPORTED_FEATURE", message)
    }

    pub fn policy(message: impl Into<String>) -> Self {
        Self::new(10, "POLICY_REJECTED", message)
    }

    pub fn json(&self) -> serde_json::Value {
        json!({
            "schema_version": 1,
            "ok": false,
            "error": {
                "code": self.code,
                "message": self.message,
                "details": self.details,
                "recovery": self.recovery,
            }
        })
    }
}

impl Display for JavelinError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for JavelinError {}

pub type Result<T> = std::result::Result<T, JavelinError>;

pub trait Context<T> {
    fn jctx(self, exit_code: u8, code: &'static str, message: impl Into<String>) -> Result<T>;
}

impl<T, E: Display> Context<T> for std::result::Result<T, E> {
    fn jctx(self, exit_code: u8, code: &'static str, message: impl Into<String>) -> Result<T> {
        self.map_err(|error| {
            JavelinError::new(exit_code, code, message.into())
                .details(json!({"cause": error.to_string()}))
        })
    }
}

#[derive(Serialize)]
pub struct Success<T: Serialize> {
    pub schema_version: u8,
    pub ok: bool,
    #[serde(flatten)]
    pub value: T,
}
