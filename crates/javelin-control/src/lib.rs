//! The in-process control plane, minimum slice: admin-token bootstrap, the device-code
//! API, idempotent repository create, and the applied-permission push to the node. State
//! lives in one directory that every process using it shares; each call holds an exclusive
//! lock on that directory. There is no HTTP server and no sign-in page or passkey yet (U8b).
//! See `docs/online-design.md` sections 4.7, 4.10 and 8 (U8).

mod control;
mod credentials;
mod device;
mod error;
mod node;
mod repo;
mod secret;
mod state;

pub use control::{ADMIN_PRINCIPAL, ControlPlane, NodeLink};
pub use credentials::Credentials;
pub use device::{AccessToken, DeviceStart, Poll, Session};
pub use error::ControlError;
pub use node::LocalNode;
pub use repo::{CreateRequest, Created, Publish};
