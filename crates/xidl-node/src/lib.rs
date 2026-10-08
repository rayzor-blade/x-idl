//! Runtime carriers for the generated Node bridge. JS values stay on their
//! owning thread; asynchronous completions carry owned Rust data.
#![allow(unsafe_op_in_unsafe_fn)]
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc, LazyLock, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

pub mod node;
pub use napi;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    // Backend callbacks outside a JS call own their scratch until that worker
    // exits. Rooted carriers and Future completions copy/retain owned data.
    static SCRATCH: RefCell<Vec<Box<dyn Any>>> = const { RefCell::new(Vec::new()) };
    static ERROR: RefCell<Option<napi::Error>> = const { RefCell::new(None) };
}
static OWNER: OnceLock<std::thread::ThreadId> = OnceLock::new();

/// One synchronous bridge call. Rejects worker-thread access and reentrancy.
pub struct Scope;
impl Scope {
    pub fn enter() -> napi::Result<Self> {
        let current = std::thread::current().id();
        if *OWNER.get_or_init(|| current) != current {
            return Err(node::invalid(
                "native bindings must be called on their owning thread",
            ));
        }
        if ACTIVE.with(|a| a.replace(true)) {
            return Err(node::invalid("reentrant native binding call"));
        }
        SCRATCH.with(|s| s.borrow_mut().clear());
        ERROR.with(|e| e.borrow_mut().take());
        Ok(Self)
    }
    pub fn finish(&self) -> napi::Result<()> {
        host::check()
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        SCRATCH.with(|s| s.borrow_mut().clear());
        ERROR.with(|e| e.borrow_mut().take());
        ACTIVE.with(|a| a.set(false));
    }
}
fn keep<T: 'static>(value: T) -> *const T {
    let value = Box::new(value);
    let ptr = &*value as *const T;
    SCRATCH.with(|s| s.borrow_mut().push(value));
    ptr
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ErrorKind {
    Type,
    #[default]
    Runtime,
}
pub mod host {
    use super::*;
    pub fn raise(kind: ErrorKind, message: &str) {
        ERROR.with(|e| {
            let mut error = e.borrow_mut();
            if error.is_none() {
                *error = Some(napi::Error::new(
                    match kind {
                        ErrorKind::Type => napi::Status::InvalidArg,
                        ErrorKind::Runtime => napi::Status::GenericFailure,
                    },
                    message.to_owned(),
                ));
            }
        });
    }
    pub fn check() -> napi::Result<()> {
        ERROR.with(|e| e.borrow_mut().take().map_or(Ok(()), Err))
    }
    pub fn blocking(_blocking: bool) {}
    pub fn agent() -> bool {
        false
    }
}

pub trait NativeEnum: Copy + Default {
    fn native(self) -> i32;
    fn from_native(value: i32) -> Option<Self>;
}
#[derive(Debug)]
pub struct Enum<T: NativeEnum>(T);
impl<T: NativeEnum> Copy for Enum<T> {}
impl<T: NativeEnum> Clone for Enum<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: NativeEnum> Enum<T> {
    pub fn get(self) -> T {
        self.0
    }
}
impl<T: NativeEnum> From<T> for Enum<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Text(*const Arc<String>);
impl Text {
    pub const NULL: Self = Self(std::ptr::null());
    pub fn new(value: &str) -> Self {
        Self::from_string(value.to_owned())
    }
    pub fn from_string(value: String) -> Self {
        Self(keep(Arc::new(value)))
    }
    pub fn as_str(&self) -> &str {
        if self.0.is_null() {
            ""
        } else {
            unsafe { &*self.0 }
        }
    }
    pub fn value(self) -> Value {
        if self.0.is_null() {
            Value::null()
        } else {
            Value::Text(self.as_str().to_owned())
        }
    }
}
#[derive(Debug)]
enum ByteStorage {
    Owned(Arc<Vec<u8>>),
    Borrowed {
        env: napi::sys::napi_env,
        value: napi::sys::napi_value,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct Buffer(*const ByteStorage);
impl Buffer {
    pub const NULL: Self = Self(std::ptr::null());
    pub fn new(value: &[u8]) -> Self {
        Self::from_vec(value.to_vec())
    }
    pub fn from_vec(value: Vec<u8>) -> Self {
        Self(keep(ByteStorage::Owned(Arc::new(value))))
    }
    pub fn len(&self) -> usize {
        unsafe { self.as_slice().len() }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub unsafe fn as_slice(&self) -> &[u8] {
        if self.0.is_null() {
            return &[];
        }
        match &*self.0 {
            ByteStorage::Owned(value) => value,
            ByteStorage::Borrowed { env, value } => match node::byte_view(*env, *value) {
                Ok((_, 0)) => &[],
                Ok((data, len)) => std::slice::from_raw_parts(data, len),
                Err(e) => {
                    host::raise(ErrorKind::Type, &e.reason);
                    &[]
                }
            },
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct BufferMut(Buffer);
impl BufferMut {
    pub const NULL: Self = Self(Buffer::NULL);
    pub fn buffer(self) -> Buffer {
        self.0
    }
    unsafe fn view(self) -> (*mut u8, usize) {
        if self.0.0.is_null() {
            return (std::ptr::null_mut(), 0);
        }
        match &*self.0.0 {
            ByteStorage::Borrowed { env, value } => match node::byte_view(*env, *value) {
                Ok(view) => view,
                Err(e) => {
                    host::raise(ErrorKind::Type, &e.reason);
                    (std::ptr::null_mut(), 0)
                }
            },
            ByteStorage::Owned(_) => {
                unreachable!("mutable buffers only borrow checked JS byte views")
            }
        }
    }
    pub unsafe fn as_slice_mut(&self) -> &mut [u8] {
        let (data, len) = self.view();
        if len == 0 {
            &mut []
        } else {
            std::slice::from_raw_parts_mut(data, len)
        }
    }
    pub unsafe fn as_mut_slice(&self) -> &mut [u8] {
        self.as_slice_mut()
    }
    pub unsafe fn as_mut_ptr(self) -> *mut u8 {
        self.view().0
    }
    pub unsafe fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}
#[derive(Debug)]
pub enum Value {
    Null,
    Text(String),
}
impl Value {
    pub fn null() -> Self {
        Self::Null
    }
}

pub trait Rootable: Copy {
    type Stored: Clone + Send + Sync + 'static;
    fn capture(self) -> Self::Stored;
    fn restore(stored: &Self::Stored) -> Self;
}
pub struct Rooted<T: Rootable>(T::Stored);
impl<T: Rootable> Clone for Rooted<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Rootable> Rooted<T> {
    pub fn new(value: T) -> Self {
        Self(value.capture())
    }
    pub fn get(&self) -> T {
        T::restore(&self.0)
    }
}
impl Rootable for Text {
    type Stored = Option<Arc<String>>;
    fn capture(self) -> Self::Stored {
        (!self.0.is_null()).then(|| unsafe { (*self.0).clone() })
    }
    fn restore(value: &Self::Stored) -> Self {
        value.as_ref().map_or(Self::NULL, |s| Self(keep(s.clone())))
    }
}
impl Rootable for Buffer {
    type Stored = Option<Arc<Vec<u8>>>;
    fn capture(self) -> Self::Stored {
        (!self.0.is_null()).then(|| unsafe {
            match &*self.0 {
                ByteStorage::Owned(v) => v.clone(),
                ByteStorage::Borrowed { .. } => Arc::new(self.as_slice().to_vec()),
            }
        })
    }
    fn restore(value: &Self::Stored) -> Self {
        value
            .as_ref()
            .map_or(Self::NULL, |v| Self(keep(ByteStorage::Owned(v.clone()))))
    }
}

type Completion = Result<Option<Box<dyn Any + Send>>, String>;
static NEXT_FUTURE: AtomicU64 = AtomicU64::new(1);
static FUTURES: LazyLock<Mutex<HashMap<u64, Weak<FutureState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub struct FutureState {
    id: u64,
    sender: Mutex<Option<tokio::sync::oneshot::Sender<Completion>>>,
    receiver: Mutex<Option<tokio::sync::oneshot::Receiver<Completion>>>,
}
impl Drop for FutureState {
    fn drop(&mut self) {
        FUTURES.lock().unwrap().remove(&self.id);
    }
}
#[derive(Debug)]
pub struct Future<T> {
    id: u64,
    marker: PhantomData<fn() -> T>,
}
impl<T> Copy for Future<T> {}
impl<T> Clone for Future<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Future<T> {
    pub const NULL: Self = Self {
        id: 0,
        marker: PhantomData,
    };
    pub fn new() -> Self {
        let id = NEXT_FUTURE.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let state = Arc::new(FutureState {
            id,
            sender: Mutex::new(Some(sender)),
            receiver: Mutex::new(Some(receiver)),
        });
        FUTURES.lock().unwrap().insert(id, Arc::downgrade(&state));
        keep(state);
        Self {
            id,
            marker: PhantomData,
        }
    }
    fn state(self) -> napi::Result<Arc<FutureState>> {
        let state = FUTURES
            .lock()
            .unwrap()
            .get(&self.id)
            .and_then(Weak::upgrade);
        state.ok_or_else(|| node::invalid("expired native future"))
    }
    fn settle(self, value: Completion) -> bool {
        self.state()
            .ok()
            .and_then(|s| s.sender.lock().unwrap().take())
            .is_some_and(|tx| tx.send(value).is_ok())
    }
    pub fn resolve_boxed(self, value: Box<T>) -> bool
    where
        T: Send + 'static,
    {
        self.settle(Ok(Some(value)))
    }
    pub fn resolve(self, value: Value) -> bool {
        match value {
            Value::Null => self.settle(Ok(None)),
            Value::Text(_) => false,
        }
    }
    pub fn reject(self, value: Value) -> bool {
        self.settle(Err(match value {
            Value::Text(s) => s,
            Value::Null => "native operation rejected".into(),
        }))
    }
}
impl<T> Default for Future<T> {
    fn default() -> Self {
        Self::NULL
    }
}
impl<T> Rootable for Future<T> {
    type Stored = Arc<FutureState>;
    fn capture(self) -> Self::Stored {
        self.state().expect("rooting a live native future")
    }
    fn restore(state: &Self::Stored) -> Self {
        Self {
            id: state.id,
            marker: PhantomData,
        }
    }
}
