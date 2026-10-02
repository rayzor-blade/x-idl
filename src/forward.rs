//! A web backend's function generated from the WebIDL member its method is
//! tagged with (`#[idl("GPUTexture.width")]`), for an adapter that does not
//! write it. The function converts its arguments to the wire's values, then
//! calls the adapter's web module, which owns the handles and the mailbox:
//!
//! - `live(interface, handle) -> bool`: whether `handle` is a live object
//!   of that WebIDL interface. A call on one that is not does nothing and
//!   returns what a failed call returns.
//! - `command(encode)`: a call with no result.
//! - `make(interface, encode) -> i32`: a call making an object of that
//!   interface, encoded with the handle it is kept under; zero when none.
//! - `ask::<T>(encode) -> Option<T>`: a call answered at once, encoded with
//!   the address of a reply record, and its answer decoded.
//! - `promise::<T>(interface, encode, resolve) -> Future<T>`: a call
//!   answered later. With an interface, the object is kept under a new
//!   handle and `resolve(future, handle)` settles the future, the handle
//!   dropped when it returns false; without one, the future resolves with
//!   nothing and `resolve` is not called.
//! - `release(interface, handle)`: forget an object, after a `destroy`.
//!
//! Interfaces are named as WebIDL names them: a receiver by the interface
//! its member belongs to, a result by the one the member returns.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Type;

use crate::convert::Values;
use crate::idl::{Model, Ty};
use crate::{BackendFn, Member, generic, type_name, wire};

/// `b`, which its adapter does not define, generated from its member.
pub(crate) fn generate(
    b: &BackendFn,
    m: &Member,
    model: &Model,
    values: &Values,
    resources: &std::collections::HashSet<String>,
) -> Result<TokenStream, String> {
    let what = format!("{}.{} ({})", m.class, b.name, m.source);
    let fail = |reason: &str| {
        Err(format!(
            "{what} cannot be generated for the web: {reason}; define `{}` in the web module, or untag it",
            b.name
        ))
    };
    let (interface, member) = m.source.split_once('.').expect("checked when declared");
    let found = wire::members(model, &m.source);
    let call = match found.as_slice() {
        [] => return fail("the wire does not carry it"),
        [call] => call,
        _ => return fail("it is overloaded"),
    };
    let receiver = m.args.first().and_then(|t| match t {
        Type::Reference(r) => type_name(&r.elem),
        _ => None,
    });
    if receiver.as_deref() != Some(&m.class.to_string()) {
        return fail("its first argument is not the receiver");
    }
    if m.args.len() - 1 > call.args.len() {
        return fail("it takes more arguments than the member");
    }

    // Arguments, in the member's order; one the method does not declare
    // must be optional, and is left out.
    let mut converted = Vec::new();
    for (i, (name, ty, optional)) in call.args.iter().enumerate() {
        let Some(declared) = m.args.get(i + 1) else {
            if !optional {
                return fail(&format!("it does not declare `{name}`"));
            }
            converted.push(quote!(None));
            continue;
        };
        let a = format_ident!("a{}", i + 1);
        let Some(value) = argument(&a, declared, ty, values, resources, &what) else {
            return fail(&format!("`{name}` does not convert"));
        };
        converted.push(if *optional && generic(declared, "Option").is_none() {
            quote!(Some(#value))
        } else {
            value
        });
    }
    let bound: Vec<_> = (0..converted.len())
        .map(|i| format_ident!("w{i}"))
        .collect();
    let method = format_ident!("{}", call.method);
    // The encoder's call: the receiver, what the call needs before its
    // arguments (a result handle, a reply), then the arguments.
    let invoke = |before: &[TokenStream]| {
        let all = std::iter::once(quote!(crate::wire::Handle(a0 as u32)))
            .chain(before.iter().cloned())
            .chain(bound.iter().map(|w| quote!(&#w)));
        quote!(e.#method(#(#all),*))
    };
    let fallback = &b.fallback;

    let body = match (&m.returns, call.promise) {
        (Some(declared), true) => {
            let Some(inner) = generic(declared, "Future") else {
                return fail("a promise is returned as a Future");
            };
            match &call.makes {
                Some(made) => {
                    let invoke_promise = invoke(&[quote!(made.unwrap_or_default()), quote!(reply)]);
                    let class =
                        format_ident!("{}", type_name(&inner).ok_or("a Future of a resource")?);
                    quote! {
                        crate::web::promise::<crate::#class>(
                            Some(#made),
                            |e, made, reply| #invoke_promise,
                            |future, handle| future.resolve_boxed(Box::new(crate::#class { handle })),
                        )
                    }
                }
                None if quote!(#inner).to_string() == "()" => {
                    let invoke = invoke(&[quote!(reply)]);
                    quote! {
                    crate::web::promise::<()>(
                        None,
                        |e, _, reply| #invoke,
                        |_, _| false,
                    )
                    }
                }
                None => return fail("only a promise of an object or of nothing is generated"),
            }
        }
        (_, true) => return fail("a promise is returned as a Future"),
        (Some(declared), false) if call.makes.is_some() => {
            if generic(declared, "Box").is_none() {
                return fail("an object is returned boxed");
            }
            let made = call.makes.as_ref().unwrap();
            let invoke = invoke(&[quote!(made)]);
            quote!(crate::web::make(#made, |e, made| #invoke))
        }
        (Some(declared), false) if call.replies => {
            let Some((wire_ty, convert)) = answer(&call.reply_ty, declared, values) else {
                return fail("its result does not convert");
            };
            let invoke = invoke(&[quote!(reply)]);
            quote! {
                match crate::web::ask::<#wire_ty>(|e, reply| #invoke) {
                    Some(v) => #convert,
                    None => #fallback,
                }
            }
        }
        (None, false) if !call.replies && call.makes.is_none() => {
            let release =
                (member == "destroy").then(|| quote!(crate::web::release(#interface, a0);));
            let invoke = invoke(&[]);
            quote! {
                crate::web::command(|e| #invoke);
                #release
            }
        }
        _ => return fail("its result does not match the member's"),
    };

    let name = &b.name;
    let params = &b.params;
    let ret = &b.ret;
    let args: Vec<_> = (0..params.len()).map(|i| format_ident!("a{i}")).collect();
    // Arguments are converted before the web module is called, so one the
    // wire cannot carry raises without the module's state taken.
    let convert = (!converted.is_empty()).then(|| {
        quote! {
            let converted = (|| -> Result<_, String> { Ok((#(#converted,)*)) })();
            let (#(#bound,)*) = match converted {
                Ok(values) => values,
                Err(message) => {
                    crate::runtime::host::raise(crate::runtime::ErrorKind::Runtime, &message);
                    return #fallback;
                }
            };
        }
    });
    Ok(quote! {
        #[allow(unused_variables, unused_parens, clippy::all)]
        pub unsafe fn #name(#(#args: #params),*) #ret {
            if !crate::web::live(#interface, a0) {
                return #fallback;
            }
            #convert
            #body
        }
    })
}

/// Argument `a`, declared as `declared`, as the wire's `ty`.
fn argument(
    a: &syn::Ident,
    declared: &Type,
    ty: &Ty,
    values: &Values,
    resources: &std::collections::HashSet<String>,
    what: &str,
) -> Option<TokenStream> {
    if let Ty::Nullable(inner) = ty
        && generic(declared, "Option").is_none()
    {
        return argument(a, declared, inner, values, resources, what).map(|x| quote!(Some(#x)));
    }
    match declared {
        // A resource arrives as its handle; a record by reference.
        Type::Reference(r) if type_name(&r.elem).is_some_and(|n| resources.contains(&n)) => {
            values.value(quote!((&#a)), &r.elem, ty, what)
        }
        Type::Reference(r) => values.value(quote!(#a), &r.elem, ty, what),
        _ if matches!(ty, Ty::String) && type_name(declared).as_deref() == Some("Text") => {
            Some(quote!(#a.as_str().to_owned()))
        }
        _ => values.value(quote!((&#a)), declared, ty, what),
    }
}

/// The wire's type of a reply carrying `ty`, and the expression turning
/// it, `v`, into the backend's result for `declared`.
fn answer(ty: &Ty, declared: &Type, values: &Values) -> Option<(TokenStream, TokenStream)> {
    let name = type_name(declared);
    let numeric = matches!(
        name.as_deref(),
        Some("i32" | "u32" | "i64" | "u64" | "f32" | "f64")
    );
    Some(match ty {
        Ty::Integer(i) if numeric => {
            let int = format_ident!("{}", wire::int_name(*i));
            (quote!(#int), quote!(v as #declared))
        }
        Ty::Float(double) if numeric => {
            let float = if *double { quote!(f64) } else { quote!(f32) };
            (float, quote!(v as #declared))
        }
        Ty::Boolean if name.as_deref() == Some("bool") => (quote!(bool), quote!(v)),
        Ty::String if name.as_deref() == Some("Text") => {
            (quote!(String), quote!(crate::runtime::Text::new(&v)))
        }
        // An imported enum's native value is its index in the IDL.
        Ty::Enum(e) if values.idl_of(declared) == Some(e.as_str()) => {
            let e = format_ident!("{e}");
            (quote!(crate::wire::#e), quote!(v as u32 as i32))
        }
        _ => return None,
    })
}
