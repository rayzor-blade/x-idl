//! Checked conversion helpers used by generated code.
use super::*;
use napi::{
    bindgen_prelude::{
        BigInt, External, FromNapiRef, FromNapiValue, ToNapiValue, Uint8Array, ValidateNapiValue,
    },
    sys,
};
use std::{ffi::CString, ptr};

pub fn invalid(message: &str) -> napi::Error {
    napi::Error::new(napi::Status::InvalidArg, message.to_owned())
}
pub unsafe fn status(code: sys::napi_status) -> napi::Result<()> {
    if code == sys::Status::napi_ok {
        Ok(())
    } else {
        Err(napi::Error::new(
            napi::Status::from(code),
            "Node-API conversion failed",
        ))
    }
}
/// Implementations may only retain owned Rust data, never a borrowed JS value.
pub trait FromNode: Sized {
    /// # Safety
    /// The environment and value must belong to this live Node-API call.
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self>;
}
pub trait ToNode: Sized {
    /// # Safety
    /// The environment must belong to the current Node thread and handle scope.
    unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value>;
}
pub unsafe fn read<T: FromNode>(env: sys::napi_env, value: sys::napi_value) -> napi::Result<T> {
    T::from_node(env, value)
}
pub unsafe fn write<T: ToNode>(env: sys::napi_env, value: T) -> napi::Result<sys::napi_value> {
    value.to_node(env)
}
unsafe fn napi_read<T: FromNapiValue + ValidateNapiValue>(
    env: sys::napi_env,
    value: sys::napi_value,
) -> napi::Result<T> {
    T::validate(env, value)?;
    T::from_napi_value(env, value)
}
pub unsafe fn nullish(env: sys::napi_env, value: sys::napi_value) -> napi::Result<bool> {
    let mut kind = 0;
    status(sys::napi_typeof(env, value, &mut kind))?;
    Ok(kind == sys::ValueType::napi_null || kind == sys::ValueType::napi_undefined)
}
pub unsafe fn optional<T>(
    env: sys::napi_env,
    value: sys::napi_value,
    read: impl FnOnce(sys::napi_value) -> napi::Result<T>,
) -> napi::Result<Option<T>> {
    if nullish(env, value)? {
        Ok(None)
    } else {
        read(value).map(Some)
    }
}
pub unsafe fn object(env: sys::napi_env, value: sys::napi_value) -> napi::Result<()> {
    let mut kind = 0;
    status(sys::napi_typeof(env, value, &mut kind))?;
    if kind != sys::ValueType::napi_object || nullish(env, value)? {
        return Err(invalid("expected a descriptor object"));
    }
    Ok(())
}
pub unsafe fn property(
    env: sys::napi_env,
    value: sys::napi_value,
    name: &str,
) -> napi::Result<sys::napi_value> {
    let name = CString::new(name).map_err(|_| invalid("invalid property name"))?;
    let mut out = ptr::null_mut();
    status(sys::napi_get_named_property(
        env,
        value,
        name.as_ptr(),
        &mut out,
    ))?;
    Ok(out)
}
pub unsafe fn new_object(env: sys::napi_env) -> napi::Result<sys::napi_value> {
    let mut out = ptr::null_mut();
    status(sys::napi_create_object(env, &mut out))?;
    Ok(out)
}
pub unsafe fn set(
    env: sys::napi_env,
    object: sys::napi_value,
    name: &str,
    value: sys::napi_value,
) -> napi::Result<()> {
    let name = CString::new(name).map_err(|_| invalid("invalid property name"))?;
    status(sys::napi_set_named_property(
        env,
        object,
        name.as_ptr(),
        value,
    ))
}
pub unsafe fn sequence<T>(
    env: sys::napi_env,
    value: sys::napi_value,
    mut read: impl FnMut(sys::napi_value) -> napi::Result<T>,
) -> napi::Result<Vec<T>> {
    if nullish(env, value)? {
        return Ok(Vec::new());
    }
    let mut is_array = false;
    status(sys::napi_is_array(env, value, &mut is_array))?;
    if !is_array {
        return Err(invalid("expected an array"));
    }
    let mut length = 0;
    status(sys::napi_get_array_length(env, value, &mut length))?;
    let mut result = Vec::with_capacity(length as usize);
    for index in 0..length {
        let mut v = ptr::null_mut();
        status(sys::napi_get_element(env, value, index, &mut v))?;
        result.push(read(v)?);
    }
    Ok(result)
}
pub unsafe fn pairs<T>(
    env: sys::napi_env,
    value: sys::napi_value,
    mut read: impl FnMut(sys::napi_value, sys::napi_value) -> napi::Result<T>,
) -> napi::Result<Vec<T>> {
    sequence(env, value, |pair| {
        let values = sequence(env, pair, Ok)?;
        if values.len() != 2 {
            return Err(invalid("expected a map entry pair"));
        }
        read(values[0], values[1])
    })
}
pub unsafe fn resource<T: 'static>(
    env: sys::napi_env,
    value: sys::napi_value,
) -> napi::Result<&'static T> {
    // napi-rs checks registry membership and TypeId before dereferencing.
    External::<T>::from_napi_ref(env, value).map(|v| v.as_ref())
}
pub unsafe fn external<T: 'static>(env: sys::napi_env, value: T) -> napi::Result<sys::napi_value> {
    External::to_napi_value(env, External::new(value))
}
pub unsafe fn bytes(env: sys::napi_env, value: &[u8]) -> napi::Result<sys::napi_value> {
    Uint8Array::to_napi_value(env, Uint8Array::new(value.to_vec()))
}
pub(crate) unsafe fn byte_view(
    env: sys::napi_env,
    value: sys::napi_value,
) -> napi::Result<(*mut u8, usize)> {
    let mut kind = 0;
    let mut len = 0;
    let mut data = ptr::null_mut();
    let mut arraybuffer = ptr::null_mut();
    let mut offset = 0;
    status(sys::napi_get_typedarray_info(
        env,
        value,
        &mut kind,
        &mut len,
        &mut data,
        &mut arraybuffer,
        &mut offset,
    ))?;
    if kind != sys::TypedarrayType::uint8_array && kind != sys::TypedarrayType::uint8_clamped_array
    {
        return Err(invalid("expected Uint8Array"));
    }
    let mut is_arraybuffer = false;
    status(sys::napi_is_arraybuffer(
        env,
        arraybuffer,
        &mut is_arraybuffer,
    ))?;
    if !is_arraybuffer {
        return Err(invalid("shared byte buffers are not supported"));
    }
    let mut detached = false;
    status(sys::napi_is_detached_arraybuffer(
        env,
        arraybuffer,
        &mut detached,
    ))?;
    if detached {
        return Err(invalid("detached byte buffer"));
    }
    Ok((data.cast(), len))
}

macro_rules! primitive {
    ($($ty:ty),*) => {$(
        impl FromNode for $ty {
            unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> { napi_read(env,value) }
        }
        impl ToNode for $ty {
            unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> { ToNapiValue::to_napi_value(env,self) }
        }
    )*};
}
primitive!(String, bool, f64, ());
impl FromNode for f32 {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        Ok(read::<f64>(env, value)? as f32)
    }
}
impl ToNode for f32 {
    unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> {
        write(env, self as f64)
    }
}
macro_rules! integer {
    ($($ty:ty),*) => {$(
        impl FromNode for $ty {
            unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
                let number: f64 = read(env,value)?;
                if !number.is_finite() || number.fract() != 0.0 || number < <$ty>::MIN as f64 || number > <$ty>::MAX as f64 { return Err(invalid("integer out of range")); }
                Ok(number as $ty)
            }
        }
        impl ToNode for $ty {
            unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> { ToNapiValue::to_napi_value(env,self) }
        }
    )*};
}
integer!(i32, u32);
impl FromNode for i64 {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        let (number, lossless) = napi_read::<BigInt>(env, value)?.get_i64();
        if !lossless {
            return Err(invalid("signed bigint out of range"));
        }
        Ok(number)
    }
}
impl FromNode for u64 {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        let (_, number, lossless) = napi_read::<BigInt>(env, value)?.get_u64();
        if !lossless {
            return Err(invalid("unsigned bigint out of range"));
        }
        Ok(number)
    }
}
macro_rules! bigint {
    ($($ty:ty),*) => {$(
        impl ToNode for $ty {
            unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> { ToNapiValue::to_napi_value(env,BigInt::from(self)) }
        }
    )*};
}
bigint!(i64, u64);
impl<T: NativeEnum> FromNode for Enum<T> {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        T::from_native(read(env, value)?)
            .map(Into::into)
            .ok_or_else(|| invalid("unknown enum value"))
    }
}
impl<T: NativeEnum> ToNode for Enum<T> {
    unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> {
        write(env, self.get().native())
    }
}
impl<T: FromNode> FromNode for Option<T> {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        optional(env, value, |v| read(env, v))
    }
}
impl<T: ToNode> ToNode for Box<T> {
    unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> {
        write(env, *self)
    }
}
impl FromNode for Text {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        Ok(Text::from_string(read::<String>(env, value)?))
    }
}
impl ToNode for Text {
    unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> {
        if self.0.is_null() {
            let mut v = ptr::null_mut();
            status(sys::napi_get_null(env, &mut v))?;
            Ok(v)
        } else {
            write(env, self.as_str().to_owned())
        }
    }
}
impl FromNode for Buffer {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        byte_view(env, value)?;
        Ok(Buffer(keep(ByteStorage::Borrowed { env, value })))
    }
}
impl ToNode for Buffer {
    unsafe fn to_node(self, env: sys::napi_env) -> napi::Result<sys::napi_value> {
        if self.0.is_null() {
            let mut v = ptr::null_mut();
            status(sys::napi_get_null(env, &mut v))?;
            Ok(v)
        } else {
            bytes(env, self.as_slice())
        }
    }
}
impl FromNode for BufferMut {
    unsafe fn from_node(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
        Ok(BufferMut(read::<Buffer>(env, value)?))
    }
}
impl<T: ToNode + Send + 'static> ToNode for Future<T> {
    unsafe fn to_node(self, raw_env: sys::napi_env) -> napi::Result<sys::napi_value> {
        let state = self.state()?;
        let receiver = state
            .receiver
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| invalid("native future already consumed"))?;
        let env = napi::Env::from_raw(raw_env);
        let promise = env.spawn_future_with_callback(
            async move {
                receiver
                    .await
                    .map_err(|_| napi::Error::from_reason("native future abandoned"))?
                    .map_err(napi::Error::from_reason)
            },
            |env, result: Option<Box<dyn Any + Send>>| {
                let scope = Scope::enter()?;
                let value = match result {
                    Some(value) => write(
                        env.raw(),
                        *value
                            .downcast::<T>()
                            .map_err(|_| invalid("wrong native future result type"))?,
                    )?,
                    None => {
                        let mut value = ptr::null_mut();
                        let code = if std::any::TypeId::of::<T>() == std::any::TypeId::of::<()>() {
                            sys::napi_get_undefined(env.raw(), &mut value)
                        } else {
                            sys::napi_get_null(env.raw(), &mut value)
                        };
                        status(code)?;
                        value
                    }
                };
                scope.finish()?;
                Ok(napi::Unknown::from_raw_unchecked(env.raw(), value))
            },
        )?;
        ToNapiValue::to_napi_value(raw_env, promise)
    }
}
