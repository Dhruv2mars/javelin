//! The in-process control plane, minimum slice: admin-token bootstrap, the device-code
//! API, idempotent repository create, and the applied-permission push to the node. State
//! lives in one directory that every process using it shares; each call holds an exclusive
//! lock on that directory. Passkeys sign the browser approval page in (U8b), which `browser`
//! serves. See `docs/online-design.md` sections 4.7, 4.10 and 8 (U8, U8b).

mod browser;
mod control;
mod credentials;
mod device;
mod error;
mod fork;
mod node;
mod passkey;
mod repo;
mod secret;
mod state;

pub use browser::{answer, serve};
pub use control::{ADMIN_PRINCIPAL, ControlPlane, ForkPlan, NodeLink};
pub use credentials::Credentials;
pub use device::{AccessToken, DeviceStart, Poll, Session};
pub use error::ControlError;
pub use fork::{ForkRequest, ForkedRepository};
pub use node::LocalNode;
pub use passkey::{BrowserSession, RelyingParty};
pub use repo::{CreateRequest, Created, Publish};
