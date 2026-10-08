//! Node-API bridges for the shared typed model.
use crate::{Declaration, RustTarget, generic, generic_pair, type_name};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use std::collections::HashSet;
use syn::ext::IdentExt;
use syn::{FnArg, Item, ReturnType, TraitItem, Type};

/// Both outputs must be generated from the same declaration and WebIDL.
pub struct Binding {
    /// Include at the adapter crate root beside `backend` and `runtime`.
    pub rust: String,
    /// TypeScript transport wrapper; import `bind` and pass the addon.
    pub typescript: String,
}

/// Generate a Node-API dispatcher and TypeScript surface from one declaration.
/// The adapter provides xidl-node carriers as `runtime` and the shared backend.
pub fn generate(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
) -> Result<Binding, String> {
    generate_with_entrypoint(namespace, declaration, webidl, "call")
}

/// Name the export when multiple bindings share one native addon.
pub fn generate_with_entrypoint(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
    entrypoint: &str,
) -> Result<Binding, String> {
    let entrypoint = crate::ident(entrypoint)?;
    let declaration = declaration.into();
    let (model, _, plugin) = crate::generate_parts(
        namespace,
        &declaration,
        webidl,
        RustTarget::Node,
        &HashSet::new(),
    )?;
    let schema = crate::typescript::schema(namespace, &model);
    let declared = syn::parse_file(&declaration.text()?).map_err(crate::error)?;
    let mut conversions = TokenStream::new();
    for record in &plugin.records {
        let class = &record.class;
        let mut fields = Vec::new();
        for (field, ty, _) in &record.fields {
            let name = field.unraw().to_string();
            let v = quote!(runtime::node::property(env, value, #name)?);
            let converted = stored(ty, v, &plugin)?;
            let converted = if type_name(ty).is_some_and(|n| plugin.unions.contains_key(&n)) {
                quote!(Some(#converted))
            } else {
                converted
            };
            fields.push(quote!(#field: #converted));
        }
        conversions.extend(quote! {
            impl runtime::node::FromNode for #class {
                unsafe fn from_node(env: napi::sys::napi_env, value: napi::sys::napi_value) -> napi::Result<Self> {
                    runtime::node::object(env, value)?;
                    Ok(Self { #(#fields),* })
                }
            }
        });
    }
    // Tagged input unions explicitly choose a schema alternative. This avoids
    // ambiguous duck typing for resources, descriptors and enum integers.
    for item in &declared.items {
        let Item::Enum(e) = item else { continue };
        let class = &e.ident;
        if let Some(variants) = plugin.unions.get(&class.to_string()) {
            let mut arms = Vec::new();
            for (variant, ty, _) in variants {
                let name = variant.unraw().to_string();
                let v = quote!(runtime::node::property(env, value, "value")?);
                let converted = stored(ty, v, &plugin)?;
                arms.push(quote!(#name => Self::#variant(#converted)));
            }
            conversions.extend(quote! {
                impl runtime::node::FromNode for #class {
                    unsafe fn from_node(env: napi::sys::napi_env, value: napi::sys::napi_value) -> napi::Result<Self> {
                        runtime::node::object(env,value)?;
                        let kind: String = runtime::node::read(env, runtime::node::property(env,value,"kind")?)?;
                        Ok(match kind.as_str() { #(#arms,)* _ => return Err(runtime::node::invalid("unknown union alternative")) })
                    }
                }
            });
        } else if let Some(variants) = plugin.variants.get(&class.to_string()) {
            let mut arms = Vec::new();
            for (variant, fields) in variants {
                let kind = variant.unraw().to_string();
                let mut values = Vec::new();
                let mut names = Vec::new();
                for (field, ty) in fields {
                    names.push(field);
                    let name = field.unraw().to_string();
                    let value = if type_name(ty).as_deref() == Some("Buffer") {
                        quote!(runtime::node::bytes(env, &#field.0)?)
                    } else if generic(ty, "Enum").is_some() {
                        quote!(runtime::node::write(env, #field.native())?)
                    } else {
                        quote!(runtime::node::write(env, #field)?)
                    };
                    values.push(quote!(runtime::node::set(env, out, #name, #value)?;));
                }
                let pattern = if names.is_empty() {
                    quote!(Self::#variant)
                } else {
                    quote!(Self::#variant { #(#names),* })
                };
                arms.push(quote!(#pattern => {
                    let out = runtime::node::new_object(env)?;
                    runtime::node::set(env,out,"kind",runtime::node::write(env,#kind.to_owned())?)?;
                    #(#values)*
                    Ok(out)
                }));
            }
            conversions.extend(quote! {
                impl runtime::node::ToNode for #class {
                    unsafe fn to_node(self, env: napi::sys::napi_env) -> napi::Result<napi::sys::napi_value> {
                        match self { #(#arms),* }
                    }
                }
            });
        }
    }
    let mut dispatch = Vec::new();
    for item in &declared.items {
        let Item::Trait(t) = item else { continue };
        let class = &t.ident;
        conversions.extend(quote! {
            impl runtime::node::FromNode for #class {
                unsafe fn from_node(env: napi::sys::napi_env, value: napi::sys::napi_value) -> napi::Result<Self> {
                    let handle = runtime::node::resource::<Self>(env, value)?;
                    Ok(Self::from_handle(handle.handle))
                }
            }
            impl runtime::node::ToNode for #class {
                unsafe fn to_node(self, env: napi::sys::napi_env) -> napi::Result<napi::sys::napi_value> {
                    runtime::node::external(env, self)
                }
            }
        });
        for method in &t.items {
            let TraitItem::Fn(f) = method else { continue };
            let name = &f.sig.ident;
            let key = proc_macro2::Literal::u32_unsuffixed(dispatch.len() as u32);
            let mut reads = Vec::new();
            let mut args = Vec::new();
            for (i, input) in f.sig.inputs.iter().enumerate() {
                let FnArg::Typed(input) = input else {
                    return Err("explicit receivers required".into());
                };
                let arg = format_ident!("a{i}");
                let ty = &input.ty;
                if let Type::Reference(r) = &**ty {
                    let owned = &r.elem;
                    reads.push(quote!(let mut #arg: #owned = runtime::node::read(env.raw(), napi::JsValue::raw(&args[#i]))?;));
                    args.push(if r.mutability.is_some() {
                        quote!(&mut #arg)
                    } else {
                        quote!(&#arg)
                    });
                } else {
                    reads.push(quote!(let #arg: #ty = runtime::node::read(env.raw(), napi::JsValue::raw(&args[#i]))?;));
                    args.push(quote!(#arg));
                }
            }
            let count = reads.len();
            let ty: Type = match &f.sig.output {
                ReturnType::Default => syn::parse_quote!(()),
                ReturnType::Type(_, ty) => (**ty).clone(),
            };
            dispatch.push(quote!(#key => {
                if args.len() != #count { return Err(runtime::node::invalid("wrong number of arguments")); }
                #(#reads)*
                let result: #ty = #class::#name(#(#args),*);
                runtime::host::check()?;
                runtime::node::write(env.raw(), result)
            }));
        }
    }
    let registration = quote! {
        #conversions
        #[napi_derive::napi]
        pub fn #entrypoint<'env>(env: napi::Env, operation: u32, args: Vec<napi::Unknown<'env>>) -> napi::Result<napi::Unknown<'env>> {
            let scope = runtime::Scope::enter()?;
            let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> napi::Result<napi::sys::napi_value> { unsafe {
                match operation { u32::MAX => runtime::node::write(env.raw(), #schema.to_owned()), #(#dispatch,)* _ => Err(runtime::node::invalid("unknown native method")) }
            }})).map_err(|_| runtime::node::invalid("native binding panicked"))??;
            scope.finish()?;
            Ok(unsafe { napi::Unknown::from_raw_unchecked(env.raw(), value) })
        }
    };
    Ok(Binding {
        rust: format!("{model}\n{registration}"),
        typescript: crate::typescript::generate(namespace, declaration, webidl)?.source,
    })
}

/// Convert the source schema's value to the typed model's stored field.
fn stored(
    ty: &Type,
    value: TokenStream,
    plugin: &crate::convert::Plugin,
) -> Result<TokenStream, String> {
    if let Some(inner) = generic(ty, "Option") {
        let converted = stored(&inner, quote!(v), plugin)?;
        return Ok(quote!(runtime::node::optional(env,#value,|v| Ok(#converted))?));
    }
    if let Some(inner) = generic(ty, "Vec") {
        let converted = stored(&inner, quote!(v), plugin)?;
        return Ok(quote!(runtime::node::sequence(env,#value,|v| Ok(#converted))?));
    }
    if let Some((key, val)) = generic_pair(ty, "Map") {
        let k = stored(&key, quote!(k), plugin)?;
        let v = stored(&val, quote!(v), plugin)?;
        return Ok(quote!(runtime::node::pairs(env,#value,|k,v| Ok((#k,#v)))?));
    }
    if let Some(e) = generic(ty, "Enum") {
        return Ok(quote!(runtime::node::read::<runtime::Enum<#e>>(env,#value)?.get().native()));
    }
    let name = type_name(ty).ok_or("named record fields required")?;
    if matches!(name.as_str(), "Text" | "Buffer") {
        Ok(quote!(runtime::Rooted::new(runtime::node::read::<#ty>(env,#value)?)))
    } else if plugin.resources.contains(&name) {
        Ok(quote!(runtime::node::read::<#ty>(env,#value)?.handle))
    } else {
        Ok(quote!(runtime::node::read::<#ty>(env,#value)?))
    }
}
