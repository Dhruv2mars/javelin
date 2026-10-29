//! The node as the Durable Object sees it, through wasm-bindgen. `worker.js` gives each
//! object's `WasmNode` its page rows (`JsPages`) and calls it inside `transactionSync`, so a
//! request's page writes commit together. A call that fails in storage, or traps, throws: the
//! object rolls the request back and opens a new node, whose caches start empty. A reply marked
//! `rollback` is rolled back the same way, and still answered.

use std::{cell::RefCell, rc::Rc};

use javelin_collaboration::RepositoryId;
use wasm_bindgen::prelude::*;

use crate::{
    pages::{self, Pages},
    service::{Node, Response},
    vfs::PageStorage,
};

#[wasm_bindgen]
extern "C" {
    /// The Durable Object's page rows, implemented in `worker.js`.
    pub type JsPages;
    #[wasm_bindgen(method, catch)]
    fn size(this: &JsPages, file: &str) -> Result<Option<f64>, JsValue>;
    #[wasm_bindgen(method, catch, js_name = setSize)]
    fn set_size(this: &JsPages, file: &str, size: f64) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    fn get(this: &JsPages, file: &str, index: f64) -> Result<Option<Vec<u8>>, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn put(this: &JsPages, file: &str, index: f64, bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    fn remove(this: &JsPages, file: &str) -> Result<(), JsValue>;
}

thread_local! {
    /// The first storage failure of the current call. SQLite reports it as an I/O error the
    /// node may turn into a protocol refusal, so the call is failed here as well.
    static FAILED: RefCell<Option<String>> = const { RefCell::new(None) };
}

struct ObjectPages(JsPages);

fn record<T>(result: Result<T, JsValue>) -> pages::Result<T> {
    result.map_err(|error| {
        let detail = error
            .as_string()
            .or_else(|| js_sys::Error::from(error).message().as_string())
            .unwrap_or_else(|| "storage call failed".into());
        FAILED.with(|failed| {
            failed.borrow_mut().get_or_insert_with(|| detail.clone());
        });
        detail
    })
}

impl Pages for ObjectPages {
    fn size(&self, file: &str) -> pages::Result<Option<u64>> {
        Ok(record(self.0.size(file))?.map(|size| size as u64))
    }

    fn set_size(&self, file: &str, size: u64) -> pages::Result<()> {
        record(self.0.set_size(file, size as f64))
    }

    fn get(&self, file: &str, index: u64) -> pages::Result<Option<Vec<u8>>> {
        record(self.0.get(file, index as f64))
    }

    fn put(&self, file: &str, index: u64, bytes: &[u8]) -> pages::Result<()> {
        record(self.0.put(file, index as f64, bytes))
    }

    fn remove(&self, file: &str) -> pages::Result<()> {
        record(self.0.remove(file))
    }
}

/// The size of one page row, for `worker.js`'s `setSize`.
#[wasm_bindgen(js_name = pageSize)]
pub fn page_size() -> u32 {
    pages::PAGE as u32
}

/// The largest request body the node reads.
#[wasm_bindgen(js_name = maxFrame)]
pub fn max_frame() -> u32 {
    javelin_protocol::limits::MAX_FRAME as u32
}

/// One answer for `worker.js`.
#[wasm_bindgen]
pub struct Reply(Response);

#[wasm_bindgen]
impl Reply {
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> u16 {
        self.0.status
    }

    #[wasm_bindgen(getter)]
    pub fn body(&self) -> Vec<u8> {
        self.0.body.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn json(&self) -> bool {
        self.0.json
    }

    #[wasm_bindgen(getter)]
    pub fn cors(&self) -> bool {
        self.0.cors
    }

    /// The call's writes must not commit: `worker.js` throws inside the transaction.
    #[wasm_bindgen(getter)]
    pub fn rollback(&self) -> bool {
        self.0.rollback
    }
}

#[wasm_bindgen]
pub struct WasmNode(Node<PageStorage>);

#[wasm_bindgen]
impl WasmNode {
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str, pages: JsPages) -> Result<WasmNode, JsError> {
        let name = RepositoryId::parse(name).map_err(|error| JsError::new(&error.to_string()))?;
        let storage = PageStorage::new(Rc::new(ObjectPages(pages)));
        Ok(Self(Node::new(name, storage, now)))
    }

    pub fn admin(&mut self, action: &str, body: &[u8]) -> Result<Reply, JsError> {
        settle(self.0.admin(action, body))
    }

    pub fn v2(
        &mut self,
        authorization: Option<String>,
        origin: bool,
        body: &[u8],
    ) -> Result<Reply, JsError> {
        settle(self.0.v2(authorization.as_deref(), origin, body))
    }
}

/// The Worker's clock, in Unix seconds. The runtime advances it between requests, not within
/// one, which is all expiry and the frontier need.
fn now() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

/// The reply, unless storage failed during the call.
fn settle(response: Response) -> Result<Reply, JsError> {
    match FAILED.with(|failed| failed.borrow_mut().take()) {
        Some(detail) => Err(JsError::new(&format!("storage failed: {detail}"))),
        None => Ok(Reply(response)),
    }
}
