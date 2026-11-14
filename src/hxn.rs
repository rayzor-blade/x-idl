//! hxn bindings: the shared model, carriers over plain Rust values, and over
//! the model the items hxn reads to generate every runtime's adapter and the
//! Haxe classes (HashLink, Ash, hxcpp, CPPIA, their wasm forms, Android and
//! iOS).
//!
//! The output is one file, which the library includes at its crate root from
//! its build script's `OUT_DIR`, beside `mod backend`:
//!
//! - `runtime`: the carriers the model and the backend use, as plain Rust.
//! - `model`: the model every target shares, as Node's target has it, with
//!   variants returned whole. The backend is installed scoped to
//!   `crate::model`.
//! - At the crate root, each resource and descriptor as an
//!   `#[hxn::interface]`, each enum as `#[hxn::data]`, and the methods, which
//!   call the model's.
//!
//! A failure the backend raises is kept by `runtime::host::raise` and
//! returned by the method as an error once the call has returned, which hxn
//! throws on the Haxe side.

use crate::{Declaration, RustTarget, error, generic, type_name};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use std::collections::{HashMap, HashSet};
use syn::ext::IdentExt;
use syn::{Attribute, FnArg, ImplItem, Item, Pat, ReturnType, TraitItem, Type};

/// The carriers, over plain Rust values. They hold what a call passes for
/// as long as the model and the backend need it.
const RUNTIME: &str = r#"
#[derive(Clone, Copy, Debug)]
pub enum ErrorKind {
    Type,
    Runtime,
}

pub mod host {
    use super::ErrorKind;
    use std::cell::RefCell;

    thread_local! {
        static RAISED: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    /// Keeps the call's first failure, which its method returns once the
    /// call is done.
    pub fn raise(kind: ErrorKind, message: &str) {
        RAISED.with(|raised| {
            let mut raised = raised.borrow_mut();
            if raised.is_none() {
                *raised = Some(match kind {
                    ErrorKind::Type => format!("type error: {message}"),
                    ErrorKind::Runtime => message.to_owned(),
                });
            }
        });
    }

    /// The failure this thread raised since the last call, if any.
    pub fn take() -> Option<String> {
        RAISED.with(|raised| raised.borrow_mut().take())
    }

    /// The binding marks a blocking method for hxn, whose adapter tells
    /// the runtime; the backend's own marks have nothing to do.
    pub fn blocking(_: bool) {}

    /// No browser agent serves a native program.
    pub fn agent(_: &str, _: usize) -> bool {
        false
    }
}

pub trait NativeEnum: Copy + Default {
    fn native(self) -> i32;
    fn from_native(value: i32) -> Option<Self>;
}

#[derive(Clone, Copy)]
pub struct Enum<T: NativeEnum>(T);

impl<T: NativeEnum> Enum<T> {
    pub fn from_native(value: i32) -> Self {
        Self(T::from_native(value).unwrap_or_default())
    }

    pub fn get(self) -> T {
        self.0
    }
}

impl<T: NativeEnum> From<T> for Enum<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

#[derive(Clone, Default)]
pub struct Text(Option<std::sync::Arc<str>>);

impl Text {
    pub const NULL: Self = Self(None);

    pub fn new(value: &str) -> Self {
        Self(Some(value.into()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_deref().unwrap_or("")
    }
}

#[derive(Clone, Default)]
pub struct Buffer(Option<std::sync::Arc<[u8]>>);

impl Buffer {
    pub const NULL: Self = Self(None);

    pub fn new(bytes: &[u8]) -> Self {
        Self(Some(bytes.into()))
    }

    pub fn len(&self) -> usize {
        self.0.as_deref().map_or(0, <[u8]>::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_deref().map_or(std::ptr::null(), <[u8]>::as_ptr)
    }

    /// # Safety
    /// None beyond the borrow: the bytes are owned.
    pub unsafe fn as_slice(&self) -> &[u8] {
        self.0.as_deref().unwrap_or(&[])
    }
}

/// Bytes the caller lends for the call to write into.
#[derive(Clone, Copy)]
pub struct BufferMut {
    bytes: *mut u8,
    len: usize,
}

impl BufferMut {
    /// Lends `bytes` for one call: the value must not outlive it.
    pub fn lent(bytes: &mut [u8]) -> Self {
        Self {
            bytes: bytes.as_mut_ptr(),
            len: bytes.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.bytes
    }
}

pub struct Future<T = ()>(std::marker::PhantomData<fn() -> T>);

impl<T> Future<T> {
    pub const NULL: Self = Self(std::marker::PhantomData);
}

#[derive(Clone)]
pub struct Rooted<T: Clone>(T);

impl<T: Clone> Rooted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn get(&self) -> T {
        self.0.clone()
    }
}
"#;

/// What the declaration says of one method beyond its signature.
#[derive(Default, Clone)]
struct Notes {
    docs: Vec<Attribute>,
    /// `#[blocking]`: the call waits, and the runtime is told so.
    blocking: bool,
}

fn docs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .cloned()
        .collect()
}

/// `takeCursorMove` as `take_cursor_move`. Every name in a declaration
/// comes back from hxn's camel case unchanged.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if index > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn snake_ident(ident: &syn::Ident) -> syn::Ident {
    format_ident!("{}", snake(&ident.unraw().to_string()))
}

/// The kinds of type the model declares, by name.
struct Kinds {
    resources: HashSet<String>,
    descriptors: HashSet<String>,
    enums: HashSet<String>,
}

impl Kinds {
    fn named(&self, ty: &Type) -> Option<(String, &'static str)> {
        let name = type_name(ty)?;
        let kind = if self.resources.contains(&name) {
            "resource"
        } else if self.descriptors.contains(&name) {
            "descriptor"
        } else if self.enums.contains(&name) {
            "enum"
        } else {
            return None;
        };
        Some((name, kind))
    }
}

fn scalar(ty: &Type) -> bool {
    matches!(
        type_name(ty).as_deref(),
        Some("bool" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64")
    ) && !matches!(ty, Type::Reference(_))
}

fn referent(ty: &Type) -> Option<(&Type, bool)> {
    match ty {
        Type::Reference(reference) => Some((&reference.elem, reference.mutability.is_some())),
        _ => None,
    }
}

/// A model parameter as hxn takes it, and how the model is given it.
fn parameter(
    name: &syn::Ident,
    ty: &Type,
    kinds: &Kinds,
) -> Result<(TokenStream, TokenStream), String> {
    let model_name = || type_name(ty).unwrap_or_default();
    if scalar(ty) {
        return Ok((quote!(#ty), quote!(#name)));
    }
    match model_name().as_str() {
        "Text" => return Ok((quote!(&str), quote!(runtime::Text::new(#name)))),
        "Buffer" => return Ok((quote!(&[u8]), quote!(runtime::Buffer::new(#name)))),
        "BufferMut" => {
            return Ok((quote!(&mut [u8]), quote!(runtime::BufferMut::lent(#name))));
        }
        _ => {}
    }
    if let Some(inner) = generic(ty, "Enum") {
        let e = format_ident!("{}", type_name(&inner).ok_or("an unnamed enum")?);
        return Ok((
            quote!(#e),
            quote!(runtime::Enum::from(model::#e::from(#name))),
        ));
    }
    if let Some(inner) = generic(ty, "Option").and_then(|o| generic(&o, "Enum")) {
        let e = format_ident!("{}", type_name(&inner).ok_or("an unnamed enum")?);
        return Ok((
            quote!(Option<#e>),
            quote!(#name.map(|value| runtime::Enum::from(model::#e::from(value)))),
        ));
    }
    if let Some((elem, mutable)) = referent(ty)
        && let Some((class, kind)) = kinds.named(elem)
    {
        let class = format_ident!("{class}");
        return match (kind, mutable) {
            ("resource", false) => Ok((
                quote!(&#class),
                quote!(&model::#class::from_handle(#name.handle)),
            )),
            ("descriptor", false) => Ok((quote!(&#class), quote!(&#name.inner))),
            _ => Err(format!("a parameter `{name}` of `&mut {class}`")),
        };
    }
    Err(format!("a parameter `{name}` of type `{}`", quote!(#ty)))
}

/// A model result as hxn returns it, and how it is made from `value`.
fn result(ty: &Type, kinds: &Kinds) -> Result<(TokenStream, TokenStream), String> {
    if scalar(ty) {
        return Ok((quote!(#ty), quote!(value)));
    }
    match type_name(ty).as_deref() {
        Some("Text") => return Ok((quote!(String), quote!(value.as_str().to_owned()))),
        Some("Buffer") => {
            return Ok((
                quote!(Vec<u8>),
                quote!(unsafe { value.as_slice() }.to_vec()),
            ));
        }
        _ => {}
    }
    if let Some(inner) = generic(ty, "Enum") {
        let e = format_ident!("{}", type_name(&inner).ok_or("an unnamed enum")?);
        return Ok((quote!(#e), quote!(#e::from(value.get()))));
    }
    if let Some(inner) = generic(ty, "Box")
        && let Some((class, kind)) = kinds.named(&inner)
    {
        let class = format_ident!("{class}");
        return match kind {
            "resource" => Ok((quote!(#class), quote!(#class { handle: value.handle }))),
            "descriptor" => Ok((quote!(#class), quote!(#class { inner: *value }))),
            _ => Err(format!("a result of `Box<{class}>`")),
        };
    }
    if let Some((class, "enum")) = kinds.named(ty) {
        let class = format_ident!("{class}");
        return Ok((quote!(#class), quote!(#class::from(value))));
    }
    Err(format!("a result of type `{}`", quote!(#ty)))
}

/// One model method as an hxn method of `class`, or `None` for the model's
/// own helpers.
fn method(
    class: &syn::Ident,
    descriptor: bool,
    function: &syn::ImplItemFn,
    notes: &Notes,
    kinds: &Kinds,
) -> Result<Option<TokenStream>, String> {
    let model_name = &function.sig.ident;
    let unraw = model_name.unraw().to_string();
    if !matches!(function.vis, syn::Visibility::Public(_))
        || ["native", "from_native", "variant", "from_handle"].contains(&unraw.as_str())
    {
        return Ok(None);
    }
    let failed = |why: String| format!("{class}.{unraw}: hxn cannot take {why}");
    let mut params = Vec::new();
    let mut passed = Vec::new();
    for (index, input) in function.sig.inputs.iter().enumerate() {
        let FnArg::Typed(typed) = input else {
            return Err(failed(String::from("a `self` receiver")));
        };
        let Pat::Ident(pattern) = &*typed.pat else {
            return Err(failed(String::from("a pattern parameter")));
        };
        let name = &pattern.ident;
        // The receiver: `this: &Window`, or `this: &mut WindowAttributes`.
        if index == 0 && name == "this" {
            let (_, mutable) =
                referent(&typed.ty).ok_or_else(|| failed("`this` by value".into()))?;
            if descriptor {
                params.push(quote!(&mut self));
                passed.push(quote!(&mut self.inner));
            } else if mutable {
                return Err(failed(String::from("a resource by `&mut`")));
            } else {
                params.push(quote!(&self));
                passed.push(quote!(&model::#class::from_handle(self.handle)));
            }
            continue;
        }
        let local = snake_ident(name);
        let (ty, given) = parameter(&local, &typed.ty, kinds).map_err(failed)?;
        params.push(quote!(#local: #ty));
        passed.push(given);
    }
    let (returns, made) = match &function.sig.output {
        ReturnType::Default => (quote!(()), quote!(value)),
        ReturnType::Type(_, ty) => {
            if descriptor && unraw == "new" {
                (quote!(Self), quote!(Self { inner: *value }))
            } else {
                result(ty, kinds).map_err(failed)?
            }
        }
    };
    let name = snake_ident(model_name);
    let docs = &notes.docs;
    let blocking = notes.blocking.then(|| quote!(#[hxn(blocking)]));
    Ok(Some(quote! {
        #(#docs)*
        #blocking
        pub fn #name(#(#params),*) -> Result<#returns, Raised> {
            let value = model::#class::#model_name(#(#passed),*);
            raised()?;
            #[allow(clippy::let_unit_value, unused_unsafe)]
            let made = #made;
            Ok(made)
        }
    }))
}

/// A model enum as hxn data, with conversions both ways. A C-like enum keeps
/// the model's numbers.
fn enumeration(
    model: &syn::ItemEnum,
    numbers: &HashMap<String, Vec<(syn::Ident, syn::Expr)>>,
    kinds: &Kinds,
    attrs: &[Attribute],
) -> Result<TokenStream, String> {
    let name = &model.ident;
    let docs = docs(attrs);
    let c_like = model.variants.iter().all(|v| v.fields.is_empty());
    if c_like {
        let values = numbers
            .get(&name.to_string())
            .ok_or_else(|| format!("{name}: the model gives it no numbers"))?;
        let variants = values
            .iter()
            .map(|(variant, number)| quote!(#variant = #number));
        let names: Vec<_> = values.iter().map(|(variant, _)| variant).collect();
        return Ok(quote! {
            #(#docs)*
            #[hxn::data]
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            pub enum #name { #(#variants),* }
            impl From<model::#name> for #name {
                fn from(value: model::#name) -> Self {
                    match value { #(model::#name::#names => Self::#names),* }
                }
            }
            impl From<#name> for model::#name {
                fn from(value: #name) -> Self {
                    match value { #(#name::#names => Self::#names),* }
                }
            }
        });
    }
    let mut variants = Vec::new();
    let mut out_arms = Vec::new();
    let mut in_arms = Vec::new();
    for variant in &model.variants {
        let v = &variant.ident;
        let syn::Fields::Named(fields) = &variant.fields else {
            if variant.fields.is_empty() {
                variants.push(quote!(#v));
                out_arms.push(quote!(model::#name::#v => Self::#v));
                in_arms.push(quote!(#name::#v => Self::#v));
                continue;
            }
            return Err(format!("{name}::{v}: an unnamed field"));
        };
        let mut declared = Vec::new();
        let mut out_fields = Vec::new();
        let mut in_fields = Vec::new();
        let names: Vec<_> = fields
            .named
            .iter()
            .map(|f| f.ident.clone().unwrap())
            .collect();
        for field in &fields.named {
            let f = field.ident.as_ref().unwrap();
            let ty = &field.ty;
            let (hxn_ty, out, back) = if type_name(ty).as_deref() == Some("VariantBytes") {
                (
                    quote!(Vec<u8>),
                    quote!(#f.0),
                    quote!(model::VariantBytes(#f)),
                )
            } else if kinds.named(ty).is_some_and(|(_, kind)| kind == "enum") {
                (quote!(#ty), quote!(#f.into()), quote!(#f.into()))
            } else if scalar(ty) || type_name(ty).as_deref() == Some("String") {
                (quote!(#ty), quote!(#f), quote!(#f))
            } else {
                return Err(format!(
                    "{name}::{v}.{f}: a field of type `{}`",
                    quote!(#ty)
                ));
            };
            declared.push(quote!(#f: #hxn_ty));
            out_fields.push(quote!(#f: #out));
            in_fields.push(quote!(#f: #back));
        }
        variants.push(quote!(#v { #(#declared),* }));
        out_arms.push(quote!(model::#name::#v { #(#names),* } => Self::#v { #(#out_fields),* }));
        in_arms.push(quote!(#name::#v { #(#names),* } => Self::#v { #(#in_fields),* }));
    }
    Ok(quote! {
        #(#docs)*
        #[hxn::data]
        #[derive(Clone, Debug, PartialEq)]
        pub enum #name { #(#variants),* }
        impl From<model::#name> for #name {
            fn from(value: model::#name) -> Self {
                match value { #(#out_arms),* }
            }
        }
        impl From<#name> for model::#name {
            fn from(value: #name) -> Self {
                match value { #(#in_arms),* }
            }
        }
    })
}

/// The numbers a C-like enum's `native` gives each variant, from the model.
fn numbers(file: &syn::File) -> HashMap<String, Vec<(syn::Ident, syn::Expr)>> {
    let mut found = HashMap::new();
    for item in &file.items {
        let Item::Impl(block) = item else { continue };
        if block.trait_.is_some() {
            continue;
        }
        let Some(name) = type_name(&block.self_ty) else {
            continue;
        };
        for member in &block.items {
            let ImplItem::Fn(function) = member else {
                continue;
            };
            if function.sig.ident != "native" {
                continue;
            }
            let Some(syn::Stmt::Expr(syn::Expr::Match(matched), _)) = function.block.stmts.first()
            else {
                continue;
            };
            let arms = matched
                .arms
                .iter()
                .filter_map(|arm| {
                    let syn::Pat::Path(path) = &arm.pat else {
                        return None;
                    };
                    let variant = path.path.segments.last()?.ident.clone();
                    Some((variant, (*arm.body).clone()))
                })
                .collect();
            found.insert(name.clone(), arms);
        }
    }
    found
}

/// The hxn binding of a declaration: one file to include at the crate root
/// beside `mod backend`, whose Haxe classes are in `package`.
pub fn generate(
    namespace: &str,
    package: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
) -> Result<String, String> {
    let declaration = declaration.into();
    let (model, _, _) = crate::generate_parts(
        namespace,
        &declaration,
        webidl,
        RustTarget::Node,
        &HashSet::new(),
    )?;
    let file = syn::parse_file(&model).map_err(error)?;
    let declared = syn::parse_file(&declaration.text()?).map_err(error)?;

    // The declaration says what each type is, documents it, and marks the
    // methods that block.
    let mut kinds = Kinds {
        resources: HashSet::new(),
        descriptors: HashSet::new(),
        enums: HashSet::new(),
    };
    let mut type_docs: HashMap<String, Vec<Attribute>> = HashMap::new();
    let mut notes: HashMap<(String, String), Notes> = HashMap::new();
    for item in &declared.items {
        match item {
            Item::Trait(item) => {
                let class = item.ident.to_string();
                kinds.resources.insert(class.clone());
                type_docs.insert(class.clone(), docs(&item.attrs));
                for member in &item.items {
                    let TraitItem::Fn(function) = member else {
                        continue;
                    };
                    notes.insert(
                        (class.clone(), function.sig.ident.unraw().to_string()),
                        Notes {
                            docs: docs(&function.attrs),
                            blocking: function.attrs.iter().any(|a| a.path().is_ident("blocking")),
                        },
                    );
                }
            }
            Item::Struct(item) => {
                type_docs.insert(item.ident.to_string(), docs(&item.attrs));
            }
            Item::Enum(item) => {
                type_docs.insert(item.ident.to_string(), docs(&item.attrs));
            }
            _ => {}
        }
    }
    for item in &file.items {
        match item {
            Item::Struct(item) if item.ident != "VariantBytes" => {
                if !kinds.resources.contains(&item.ident.to_string()) {
                    kinds.descriptors.insert(item.ident.to_string());
                }
            }
            Item::Enum(item) => {
                kinds.enums.insert(item.ident.to_string());
            }
            _ => {}
        }
    }

    let numbers = numbers(&file);
    let mut out = TokenStream::new();
    for item in &file.items {
        let Item::Enum(model) = item else { continue };
        let attrs = type_docs
            .get(&model.ident.to_string())
            .cloned()
            .unwrap_or_default();
        out.extend(enumeration(model, &numbers, &kinds, &attrs)?);
    }
    let mut classes = Vec::new();
    for item in &file.items {
        let Item::Struct(model) = item else { continue };
        let name = model.ident.to_string();
        if kinds.resources.contains(&name) || kinds.descriptors.contains(&name) {
            classes.push(model.ident.clone());
        }
    }
    for class in &classes {
        let descriptor = kinds.descriptors.contains(&class.to_string());
        let docs = type_docs
            .get(&class.to_string())
            .cloned()
            .unwrap_or_default();
        let held = if descriptor {
            quote!(inner: model::#class)
        } else {
            quote!(handle: i32)
        };
        let mut methods = Vec::new();
        for item in &file.items {
            let Item::Impl(block) = item else { continue };
            if block.trait_.is_some()
                || type_name(&block.self_ty).as_deref() != Some(&class.to_string())
            {
                continue;
            }
            for member in &block.items {
                let ImplItem::Fn(function) = member else {
                    continue;
                };
                let key = (class.to_string(), function.sig.ident.unraw().to_string());
                let notes = notes.get(&key).cloned().unwrap_or_default();
                methods.extend(method(class, descriptor, function, &notes, &kinds)?);
            }
        }
        out.extend(quote! {
            #(#docs)*
            #[hxn::interface]
            pub struct #class { #held }
            #[hxn::methods]
            impl #class { #(#methods)* }
        });
    }

    let runtime: TokenStream = RUNTIME.parse().map_err(error)?;
    let model: TokenStream = model.parse().map_err(error)?;
    let file = quote! {
        hxn::package!(#package);

        #[allow(dead_code, clippy::all)]
        pub mod runtime { #runtime }

        #[allow(dead_code, non_snake_case, unused_imports, unused_unsafe, improper_ctypes_definitions, clippy::all)]
        pub mod model {
            use super::backend;
            // A backend installed scoped to the model finds the carriers here.
            pub use super::runtime;
            use super::runtime::{Buffer, BufferMut, Enum, ErrorKind, Future, NativeEnum, Rooted, Text, host};
            #model
        }

        /// A failure the backend raised during a call.
        #[derive(Debug)]
        pub struct Raised(String);

        impl std::fmt::Display for Raised {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        fn raised() -> Result<(), Raised> {
            match runtime::host::take() {
                Some(message) => Err(Raised(message)),
                None => Ok(()),
            }
        }

        #out
    };
    Ok(file.to_string())
}

#[cfg(test)]
mod tests {
    const API: &str = r#"
        /// Light or dark.
        enum Theme {
            Light,
            Dark,
        }

        enum Event {
            None,
            Resized { width: i64, height: i64 },
            ThemeChanged { theme: Enum<Theme> },
        }

        /// What a window opens with.
        struct Attributes {
            title: Option<Text>,
            theme: Option<Enum<Theme>>,
        }

        /// A window.
        trait Window {
            #[native(window_open)]
            fn open(attributes: &Attributes) -> Box<Window>;
            /// The next event, waiting up to `timeout` seconds.
            #[blocking]
            #[native(window_wait)]
            fn wait(this: &Window, timeout: f64) -> Event;
            #[native(window_title)]
            fn title(this: &Window) -> Text;
            #[native(window_set_theme)]
            fn setTheme(this: &Window, theme: Option<Enum<Theme>>);
            #[native(window_close)]
            fn close(this: &Window);
        }
    "#;

    fn binding() -> String {
        let out = super::generate("window", "window", API, "").unwrap();
        syn::parse_file(&out).expect("the binding parses");
        out
    }

    #[test]
    fn resources_and_descriptors_become_interfaces_over_the_model() {
        let out = binding();
        assert!(out.contains("hxn :: package ! (\"window\")"));
        assert!(out.contains("# [hxn :: interface] pub struct Window { handle : i32 }"));
        assert!(out.contains(
            "# [hxn :: interface] pub struct Attributes { inner : model :: Attributes }"
        ));
        assert!(out.contains(
            "pub fn open (attributes : & Attributes) -> Result < Window , Raised > { let value = model :: Window :: open (& attributes . inner) ;"
        ));
        assert!(
            out.contains("pub fn title (& mut self , value : & str) -> Result < () , Raised >")
        );
        assert!(out.contains(
            "pub fn set_theme (& self , theme : Option < Theme >) -> Result < () , Raised >"
        ));
        assert!(out.contains("Ok (made)"));
        assert!(out.contains("raised () ?"));
    }

    #[test]
    fn a_blocking_method_is_marked_and_its_docs_are_kept() {
        let out = binding();
        assert!(out.contains(
            "# [doc = \" The next event, waiting up to `timeout` seconds.\"] # [hxn (blocking)] pub fn wait (& self , timeout : f64) -> Result < Event , Raised >"
        ));
        assert!(out.contains("# [doc = \" A window.\"] # [hxn :: interface] pub struct Window"));
    }

    #[test]
    fn enums_are_data_that_keeps_the_models_numbers() {
        let out = binding();
        assert!(out.contains("# [doc = \" Light or dark.\"] # [hxn :: data]"));
        assert!(out.contains("pub enum Theme { Light = 0 , Dark = 1 }"));
        assert!(out.contains("pub enum Event { None , Resized { width : i64 , height : i64 } , ThemeChanged { theme : Theme } }"));
        assert!(out.contains("model :: Event :: ThemeChanged { theme } => Self :: ThemeChanged { theme : theme . into () }"));
    }
}
