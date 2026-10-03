mod convert;
mod forward;
pub mod haxe;
pub mod haxe_js;
pub mod idl;
pub mod wire;

/// Generate complete conventional Haxe surface for one runtime.
pub fn haxe(
    namespace: &str,
    declaration: impl Into<Declaration>,
    idl_path: &str,
    runtime: haxe::Runtime,
) -> Result<Vec<haxe::File>, String> {
    let idl = std::fs::read_to_string(idl_path).map_err(error)?;
    haxe::generate(namespace, declaration, &idl, runtime)
}

use proc_macro2::TokenStream;
use quote::quote;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use syn::ext::IdentExt;
use syn::{FnArg, GenericArgument, Item, PathArguments, ReturnType, TraitItem, Type};

/// The declaration a binding is generated from: its text, a file holding
/// it, or none.
#[derive(Clone, Debug, Default)]
pub enum Declaration {
    #[default]
    None,
    Text(String),
    Path(PathBuf),
}

impl Declaration {
    fn text(&self) -> Result<String, String> {
        match self {
            Declaration::None => Ok(String::new()),
            Declaration::Text(text) => Ok(text.clone()),
            Declaration::Path(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("reading {}: {e}", path.display())),
        }
    }
}

impl From<&str> for Declaration {
    fn from(text: &str) -> Self {
        Declaration::Text(text.to_owned())
    }
}

impl From<String> for Declaration {
    fn from(text: String) -> Self {
        Declaration::Text(text)
    }
}

impl From<PathBuf> for Declaration {
    fn from(path: PathBuf) -> Self {
        Declaration::Path(path)
    }
}

impl From<Option<PathBuf>> for Declaration {
    fn from(path: Option<PathBuf>) -> Self {
        path.map_or(Declaration::None, Declaration::Path)
    }
}

fn error(message: impl std::fmt::Display) -> String {
    message.to_string()
}
fn idl_name(attrs: &[syn::Attribute]) -> Result<Option<String>, String> {
    attrs
        .iter()
        .find(|a| a.path().is_ident("idl"))
        .map(|a| {
            a.parse_args::<syn::LitStr>()
                .map(|s| s.value())
                .map_err(error)
        })
        .transpose()
}
/// `#[extension]` marks a member the backend has beyond the WebIDL source:
/// an extra record field, enum value or union alternative.
fn extension(attrs: &[syn::Attribute]) -> Result<bool, String> {
    let mut found = false;
    for attr in attrs {
        if attr.path().is_ident("extension") {
            attr.meta.require_path_only().map_err(error)?;
            found = true;
        } else if !attr.path().is_ident("doc") {
            return Err("the only member attribute is #[extension]".into());
        }
    }
    Ok(found)
}

/// A WebIDL member may be a Rust keyword (`type`); it stays itself as a raw
/// identifier, which `plugin!` exports without the `r#`.
fn ident(name: &str) -> Result<syn::Ident, String> {
    syn::parse_str(name).or_else(|e| {
        let raw = !matches!(name, "self" | "Self" | "super" | "crate" | "_")
            && syn::parse_str::<syn::Ident>(&format!("r#{name}")).is_ok();
        if raw {
            Ok(syn::Ident::new_raw(name, proc_macro2::Span::call_site()))
        } else {
            Err(error(e))
        }
    })
}
fn pascal(name: &str) -> String {
    let mut result = String::new();
    for word in name.split('-') {
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            result.extend(c.to_uppercase());
            result.extend(chars);
        }
    }
    if result.starts_with(|c: char| c.is_ascii_digit()) {
        result.insert(0, 'D');
    }
    result
}

/// Tokenize just enough WebIDL to extract enums and integer namespaces.
/// Comments and quoted braces never affect declaration boundaries.
fn tokens(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = ' ';
            let mut closed = false;
            for c in chars.by_ref() {
                if previous == '*' && c == '/' {
                    closed = true;
                    break;
                }
                previous = c;
            }
            if !closed {
                return Err("unterminated WebIDL comment".into());
            }
            continue;
        }
        let mut token = c.to_string();
        if c == '"' {
            let mut escaped = false;
            let mut closed = false;
            for c in chars.by_ref() {
                token.push(c);
                if c == '"' && !escaped {
                    closed = true;
                    break;
                }
                escaped = c == '\\' && !escaped;
            }
            if !closed {
                return Err("unterminated WebIDL string".into());
            }
        } else if c.is_ascii_alphanumeric() || c == '_' {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                token.push(chars.next().unwrap());
            }
        }
        out.push(token);
    }
    Ok(out)
}
fn body<'a>(tokens: &'a [String], kind: &str, name: &str) -> Result<&'a [String], String> {
    let matches: Vec<_> = tokens
        .windows(2)
        .enumerate()
        .filter(|(_, t)| t[0] == kind && t[1] == name)
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL {kind} {name}, found {}",
            matches.len()
        ));
    }
    let declaration = matches[0].0;
    let open = tokens[declaration + 2..]
        .iter()
        .position(|s| s == "{")
        .map(|i| declaration + 2 + i)
        .ok_or_else(|| format!("{kind} {name} has no body"))?;
    let mut depth = 0usize;
    for (offset, token) in tokens[open..].iter().enumerate() {
        match token.as_str() {
            "{" => depth += 1,
            "}" => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&tokens[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!("unclosed {name}"))
}

fn declaration_prefix<'a>(
    tokens: &'a [String],
    kind: &str,
    name: &str,
) -> Result<&'a [String], String> {
    let matches: Vec<_> = tokens
        .windows(2)
        .enumerate()
        .filter(|(_, t)| t[0] == kind && t[1] == name)
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL {kind} {name}, found {}",
            matches.len()
        ));
    }
    let start = matches[0].0 + 2;
    let end = tokens[start..]
        .iter()
        .position(|s| s == "{")
        .ok_or_else(|| format!("{kind} {name} has no body"))?;
    Ok(&tokens[start..start + end])
}

fn statements(tokens: &[String]) -> Vec<&[String]> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut brace = 0usize;
    let mut bracket = 0usize;
    for (i, token) in tokens.iter().enumerate() {
        match token.as_str() {
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "{" => brace += 1,
            "}" => brace = brace.saturating_sub(1),
            "[" => bracket += 1,
            "]" => bracket = bracket.saturating_sub(1),
            ";" if angle == 0 && paren == 0 && brace == 0 && bracket == 0 => {
                if start < i {
                    result.push(&tokens[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    result
}

fn typedefs(tokens: &[String]) -> HashMap<String, Vec<String>> {
    statements(tokens)
        .into_iter()
        .filter_map(|statement| {
            let start = statement.iter().position(|token| token == "typedef")? + 1;
            let alias = statement.last()?.clone();
            Some((alias, statement[start..statement.len() - 1].to_vec()))
        })
        .collect()
}

fn strip_attributes(mut ty: &[String]) -> &[String] {
    while ty.first().is_some_and(|token| token == "[") {
        let mut depth = 0usize;
        let Some(end) = ty.iter().position(|token| {
            if token == "[" {
                depth += 1;
            } else if token == "]" {
                depth -= 1;
            }
            depth == 0
        }) else {
            break;
        };
        ty = &ty[end + 1..];
    }
    ty
}

fn idl_generic<'a>(tokens: &'a [String], name: &str) -> Option<Vec<&'a [String]>> {
    if tokens.len() < 4 || tokens[0] != name || tokens[1] != "<" || tokens.last()? != ">" {
        return None;
    }
    let inner = &tokens[2..tokens.len() - 1];
    let mut parts = Vec::new();
    let mut start = 0;
    let mut angle = 0usize;
    let mut paren = 0usize;
    for (i, token) in inner.iter().enumerate() {
        match token.as_str() {
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "," if angle == 0 && paren == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    Some(parts)
}

/// The alternatives of a parenthesised WebIDL union, `undefined` and `null`
/// left out; `None` for a type that is not a union.
fn union_alternatives(tokens: &[String]) -> Option<Vec<&[String]>> {
    if tokens.first()? != "(" || tokens.last()? != ")" {
        return None;
    }
    let inner = &tokens[1..tokens.len() - 1];
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, token) in inner.iter().enumerate() {
        match token.as_str() {
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "or" if angle == 0 && paren == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    Some(
        parts
            .into_iter()
            .filter(|part| *part != ["undefined"] && *part != ["null"])
            .collect(),
    )
}

fn idl_union_without_undefined(tokens: &[String]) -> Option<&[String]> {
    let concrete = union_alternatives(tokens)?;
    (concrete.len() == 1).then(|| concrete[0])
}

fn idl_type(
    tokens: &[String],
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
    resolving: &mut HashSet<String>,
) -> Result<Type, String> {
    let tokens = strip_attributes(tokens);
    if tokens.last().is_some_and(|token| token == "?") {
        let inner = idl_type(&tokens[..tokens.len() - 1], aliases, named, resolving)?;
        return Ok(syn::parse_quote!(Option<#inner>));
    }
    if let Some(inner) = idl_union_without_undefined(tokens) {
        let inner = idl_type(inner, aliases, named, resolving)?;
        return Ok(syn::parse_quote!(Option<#inner>));
    }
    if let Some(parts) = idl_generic(tokens, "sequence") {
        if parts.len() != 1 {
            return Err("WebIDL sequence needs one element type".into());
        }
        let inner = idl_type(parts[0], aliases, named, resolving)?;
        return Ok(syn::parse_quote!(Vec<#inner>));
    }
    if let Some(parts) = idl_generic(tokens, "record") {
        if parts.len() != 2 {
            return Err("WebIDL record needs key and value types".into());
        }
        let key = idl_type(parts[0], aliases, named, resolving)?;
        let mut value = idl_type(parts[1], aliases, named, resolving)?;
        if let Some(inner) = generic(&value, "Option") {
            value = inner;
        }
        return Ok(syn::parse_quote!(Map<#key, #value>));
    }
    if let Some(parts) = idl_generic(tokens, "Promise") {
        if parts.len() != 1 {
            return Err("WebIDL Promise needs one result type".into());
        }
        let mut inner = idl_type(parts[0], aliases, named, resolving)?;
        // A rejected future represents the WebIDL operation's absence/error
        // path. Caribou frontends therefore expose a nullable Promise result
        // as the same typed result as a non-null Promise.
        if let Some(value) = generic(&inner, "Option") {
            inner = value;
        }
        return Ok(syn::parse_quote!(Future<#inner>));
    }
    let spelling = tokens.join(" ");
    let primitive = match spelling.as_str() {
        "undefined" => Some(syn::parse_quote!(())),
        "boolean" => Some(syn::parse_quote!(bool)),
        "byte" | "octet" | "short" | "unsigned short" | "long" | "unsigned long" => {
            Some(syn::parse_quote!(i32))
        }
        "long long" | "unsigned long long" => Some(syn::parse_quote!(i64)),
        "float" => Some(syn::parse_quote!(f32)),
        "double" => Some(syn::parse_quote!(f64)),
        "DOMString" | "USVString" | "ByteString" => Some(syn::parse_quote!(Text)),
        _ => None,
    };
    if let Some(primitive) = primitive {
        return Ok(primitive);
    }
    if tokens.len() == 1 {
        let name = &tokens[0];
        if let Some(target) = named.get(name) {
            return Ok(target.clone());
        }
        if let Some(alias) = aliases.get(name) {
            if !resolving.insert(name.clone()) {
                return Err(format!("recursive WebIDL typedef {name}"));
            }
            let result = idl_type(alias, aliases, named, resolving);
            resolving.remove(name);
            return result;
        }
    }
    Err(format!("unsupported WebIDL type {spelling}"))
}

/// Every body of `kind name`, its partial definitions included. A mixin's
/// is found as `mixin name`.
fn bodies<'a>(tokens: &'a [String], kind: &str, name: &str) -> Vec<&'a [String]> {
    let mut found = Vec::new();
    for (at, pair) in tokens.windows(2).enumerate() {
        if pair[0] != kind || pair[1] != name {
            continue;
        }
        let Some(open) = tokens[at + 2..].iter().position(|s| s == "{") else {
            continue;
        };
        let open = at + 2 + open;
        let mut depth = 0usize;
        for (offset, token) in tokens[open..].iter().enumerate() {
            match token.as_str() {
                "{" => depth += 1,
                "}" => {
                    depth -= 1;
                    if depth == 0 {
                        found.push(&tokens[open + 1..open + offset]);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    found
}

/// The type a WebIDL member gives: an operation's return or an
/// attribute's type. `source` is `Interface.member`, and an interface's
/// members include its partial definitions' and its mixins'.
fn member_return(
    tokens: &[String],
    source: &str,
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
) -> Result<Type, String> {
    let (interface, member) = source
        .split_once('.')
        .ok_or_else(|| format!("WebIDL member {source} must be Interface.member"))?;
    let mut scopes = bodies(tokens, "interface", interface);
    if scopes.is_empty() {
        return Err(format!("no WebIDL interface {interface}"));
    }
    for window in tokens.windows(3) {
        if window[0] == interface && window[1] == "includes" {
            scopes.extend(bodies(tokens, "mixin", &window[2]));
        }
    }
    let mut matches: Vec<&[String]> = Vec::new();
    for scope in scopes {
        for statement in statements(scope) {
            if let Some(at) = statement
                .windows(2)
                .position(|part| part[0] == member && part[1] == "(")
            {
                matches.push(&statement[..at]);
            } else if let Some(attribute) = statement.iter().position(|t| t == "attribute")
                && statement.len() > attribute + 2
                && statement.last().is_some_and(|t| t == member)
            {
                matches.push(&statement[attribute + 1..statement.len() - 1]);
            }
        }
    }
    // Overloads may share a return type.
    matches.dedup_by(|a, b| a == b);
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL member {source}, found {}",
            matches.len()
        ));
    }
    let tokens: Vec<String> = matches[0]
        .iter()
        .filter(|t| !matches!(t.as_str(), "static" | "readonly" | "inherit"))
        .cloned()
        .collect();
    idl_type(&tokens, aliases, named, &mut HashSet::new())
}

/// Whether a method tagged with WebIDL member `source` returns what that
/// member gives. A resource is returned boxed, and a resource that imports
/// no WebIDL interface stands for whichever the member names.
fn check_member(
    tokens: &[String],
    source: &str,
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
    declared: &Type,
    untagged: &HashSet<String>,
) -> Result<(), String> {
    let declared = generic(declared, "Box").unwrap_or_else(|| declared.clone());
    match member_return(tokens, source, aliases, named) {
        Ok(imported) if quote!(#imported).to_string() == quote!(#declared).to_string() => Ok(()),
        Ok(imported) => Err(format!(
            "returns {}, but {source} maps to {}",
            quote!(#declared),
            quote!(#imported)
        )),
        Err(e) if e.starts_with("unsupported WebIDL type") => {
            let resource = generic(&declared, "Future").unwrap_or(declared);
            if type_name(&resource).is_some_and(|n| untagged.contains(&n)) {
                Ok(())
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

fn dictionary_fields(
    tokens: &[String],
    name: &str,
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
    overrides: &HashMap<String, Type>,
) -> Result<Vec<(syn::Ident, Type)>, String> {
    let mut fields = Vec::new();
    let prefix = declaration_prefix(tokens, "dictionary", name)?;
    if prefix.first().is_some_and(|token| token == ":") {
        let parent = prefix
            .get(1)
            .ok_or_else(|| format!("dictionary {name} has no parent name"))?;
        fields.extend(dictionary_fields(
            tokens, parent, aliases, named, overrides,
        )?);
    } else if !prefix.is_empty() {
        return Err(format!("unsupported dictionary declaration for {name}"));
    }
    for statement in statements(body(tokens, "dictionary", name)?) {
        let required = statement.first().is_some_and(|token| token == "required");
        let statement = if required { &statement[1..] } else { statement };
        let before_default = statement
            .iter()
            .position(|token| token == "=")
            .map_or(statement, |at| &statement[..at]);
        let (field, ty) = before_default
            .split_last()
            .ok_or_else(|| format!("empty member in dictionary {name}"))?;
        let field = ident(field)?;
        let mut ty = if let Some(override_type) = overrides.get(&field.to_string()) {
            override_type.clone()
        } else {
            idl_type(ty, aliases, named, &mut HashSet::new())?
        };
        // An optional member is Option<_> unless an override already says so.
        if !required
            && generic(&ty, "Vec").is_none()
            && generic_pair(&ty, "Map").is_none()
            && generic(&ty, "Option").is_none()
        {
            ty = syn::parse_quote!(Option<#ty>);
        }
        fields.push((field, ty));
    }
    Ok(fields)
}
/// Declared variants: each variant's name and its named fields, in order.
pub(crate) type Variants = Vec<(syn::Ident, Vec<(syn::Ident, Type)>)>;

/// The variants `e` declares, whose fields are numbers, `bool`, `Text`,
/// `Buffer`, an `Enum`, or variants named in `kinds`.
fn declared_variants(e: &syn::ItemEnum, kinds: &HashSet<String>) -> Result<Variants, String> {
    let name = &e.ident;
    if !e.generics.params.is_empty() {
        return Err(format!("variants {name} cannot be generic"));
    }
    let mut out = Vec::new();
    for v in &e.variants {
        if v.discriminant.is_some() || extension(&v.attrs)? {
            return Err(format!(
                "{name}::{} takes no value or #[extension]",
                v.ident
            ));
        }
        let fields = match &v.fields {
            syn::Fields::Unit => Vec::new(),
            syn::Fields::Named(fields) => fields
                .named
                .iter()
                .map(|f| (f.ident.clone().expect("a named field"), f.ty.clone()))
                .collect(),
            syn::Fields::Unnamed(_) => {
                return Err(format!("{name}::{} names its fields", v.ident));
            }
        };
        for (field, ty) in &fields {
            let carried = generic(ty, "Enum").is_some()
                || type_name(ty).is_some_and(|n| {
                    kinds.contains(&n)
                        || matches!(
                            n.as_str(),
                            "i32" | "i64" | "f32" | "f64" | "bool" | "Text" | "Buffer"
                        )
                });
            if !carried {
                return Err(format!(
                    "{name}::{}.{field} is not a number, bool, Text, Buffer, Enum or variants",
                    v.ident
                ));
            }
        }
        out.push((v.ident.clone(), fields));
    }
    if out.is_empty() {
        return Err(format!("empty variants {name}"));
    }
    Ok(out)
}

/// An error if variants hold themselves, directly or through others: a
/// value of them could never end.
fn acyclic(variants: &HashMap<String, Variants>) -> Result<(), String> {
    fn visit(
        name: &str,
        variants: &HashMap<String, Variants>,
        open: &mut Vec<String>,
        done: &mut HashSet<String>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if open.iter().any(|n| n == name) {
            return Err(format!("variants {name} hold themselves"));
        }
        open.push(name.to_owned());
        for (_, fields) in &variants[name] {
            for (_, ty) in fields {
                if let Some(nested) = type_name(ty).filter(|n| variants.contains_key(n)) {
                    visit(&nested, variants, open, done)?;
                }
            }
        }
        open.pop();
        done.insert(name.to_owned());
        Ok(())
    }
    let mut done = HashSet::new();
    for name in variants.keys() {
        visit(name, variants, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

/// A variant field as its enum holds it: text as a `String`, bytes as
/// `VariantBytes`, an enum as its value.
fn variant_storage(ty: &Type) -> TokenStream {
    if let Some(enumeration) = generic(ty, "Enum") {
        return quote!(#enumeration);
    }
    match type_name(ty).as_deref() {
        Some("Text") => quote!(String),
        Some("Buffer") => quote!(VariantBytes),
        _ => quote!(#ty),
    }
}

/// The getter that reads `field` of `variant` beneath `prefix`: the
/// variants type's prefix, then each variant and field on the way down.
pub(crate) fn variant_getter(prefix: &str, variant: &syn::Ident, field: &syn::Ident) -> String {
    let field: String = field
        .unraw()
        .to_string()
        .split('_')
        .map(pascal)
        .collect();
    format!("{prefix}{variant}{field}")
}

/// What the getters of a variants type's fields begin with: its name, in
/// lower camel case.
pub(crate) fn variant_prefix(kind: &str) -> String {
    let mut chars = kind.chars();
    chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

/// One step down into a variants value: its type, the variant, the field.
type Step = (syn::Ident, syn::Ident, syn::Ident);

/// `read` of `found`, the value at the end of `path` in what `slot` holds,
/// or `miss` where that value has another shape.
fn reach(slot: &syn::Ident, path: &[Step], read: TokenStream, miss: &TokenStream) -> TokenStream {
    let mut body = read;
    for (owner, variant, field) in path.iter().rev() {
        body = quote! {
            match found {
                #owner::#variant { #field: found, .. } => #body,
                _ => #miss,
            }
        };
    }
    quote! {
        #slot.with(|slot| {
            let held = slot.borrow();
            let found = &*held;
            #body
        })
    }
}

/// The getters for every field of the variants `name` at `path` in what a
/// call kept in `slot`: an index for each nested value, which has getters
/// of its own, and one getter for each other field.
#[allow(clippy::too_many_arguments)]
fn variant_getters(
    class: &syn::Ident,
    slot: &syn::Ident,
    prefix: &str,
    name: &str,
    path: &[Step],
    variants: &HashMap<String, Variants>,
    names: &mut HashSet<String>,
    methods: &mut TokenStream,
) -> Result<(), String> {
    let owner = ident(name)?;
    for (variant, fields) in &variants[name] {
        for (field, ty) in fields {
            let getter = variant_getter(prefix, variant, field);
            let mut step = path.to_vec();
            step.push((owner.clone(), variant.clone(), field.clone()));
            if let Some(nested) = type_name(ty).filter(|n| variants.contains_key(n)) {
                let index = ident(&format!("{getter}Variant"))?;
                if !names.insert(index.to_string()) {
                    return Err(format!("generated method {class}.{index} is duplicated"));
                }
                let read = reach(slot, &step, quote!(found.variant()), &quote!(0));
                methods.extend(quote! {
                    #[allow(unreachable_patterns)]
                    pub extern "C" fn #index() -> i32 { #read }
                });
                variant_getters(
                    class, slot, &getter, &nested, &step, variants, names, methods,
                )?;
                continue;
            }
            let getter = ident(&getter)?;
            if !names.insert(getter.to_string()) {
                return Err(format!("generated method {class}.{getter} is duplicated"));
            }
            let (ret, read, miss) = if let Some(e) = generic(ty, "Enum") {
                (
                    quote!(Enum<#e>),
                    quote!((*found).into()),
                    quote!(#e::default().into()),
                )
            } else {
                match type_name(ty).as_deref() {
                    Some("Text") => (quote!(Text), quote!(Text::new(found)), quote!(Text::NULL)),
                    Some("Buffer") => (
                        quote!(Buffer),
                        quote!(Buffer::new(&found.0)),
                        quote!(Buffer::NULL),
                    ),
                    _ => (quote!(#ty), quote!(*found), quote!(Default::default())),
                }
            };
            let body = reach(slot, &step, read, &miss);
            methods.extend(quote! {
                #[allow(unreachable_patterns)]
                pub extern "C" fn #getter() -> #ret { #body }
            });
        }
    }
    Ok(())
}

/// An integer literal discriminant, possibly negative.
fn discriminant(expr: &syn::Expr) -> Option<i32> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(value),
            ..
        }) => value.base10_parse().ok(),
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => discriminant(expr)?.checked_neg(),
        syn::Expr::Paren(inner) => discriminant(&inner.expr),
        _ => None,
    }
}

fn enum_values(tokens: &[String], name: &str) -> Result<Vec<String>, String> {
    let body = body(tokens, "enum", name)?;
    let mut values = Vec::new();
    for (i, token) in body.iter().enumerate() {
        if i % 2 == 1 {
            if token != "," {
                return Err(format!("expected comma in {name}"));
            }
        } else {
            values.push(syn::parse_str::<syn::LitStr>(token).map_err(error)?.value());
        }
    }
    if values.is_empty() {
        return Err(format!("empty enum {name}"));
    }
    Ok(values)
}

/// Names of readonly attributes on an interface. Finite interface catalogs
/// such as `GPUSupportedLimits` can therefore generate a Caribou enum without
/// copying their member list into the declaration.
fn readonly_attribute_names(tokens: &[String], name: &str) -> Result<Vec<String>, String> {
    let body = body(tokens, "interface", name)?;
    let mut values = Vec::new();
    for statement in body.split(|token| token == ";").filter(|s| !s.is_empty()) {
        if statement.first().is_some_and(|token| token == "readonly")
            && statement.get(1).is_some_and(|token| token == "attribute")
        {
            values.push(
                statement
                    .last()
                    .ok_or_else(|| format!("attribute without a name in {name}"))?
                    .clone(),
            );
        }
    }
    if values.is_empty() {
        return Err(format!("interface {name} has no readonly attributes"));
    }
    Ok(values)
}
fn generic(ty: &Type, name: &str) -> Option<Type> {
    let Type::Path(p) = ty else { return None };
    let segment = p.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    if args.args.len() != 1 {
        return None;
    }
    match args.args.first()? {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    }
}
fn generic_pair(ty: &Type, name: &str) -> Option<(Type, Type)> {
    let Type::Path(p) = ty else { return None };
    let segment = p.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut types = args.args.iter().filter_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty.clone()),
        _ => None,
    });
    let pair = (types.next()?, types.next()?);
    types.next().is_none().then_some(pair)
}
fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(p) = ty else { return None };
    p.path.get_ident().map(ToString::to_string)
}
fn scalar(ty: &Type) -> bool {
    if generic(ty, "Future").is_some() {
        return true;
    }
    type_name(ty).is_some_and(|s| {
        matches!(
            s.as_str(),
            "i32"
                | "u32"
                | "i64"
                | "f32"
                | "f64"
                | "bool"
                | "Text"
                | "Buffer"
                | "BufferMut"
                | "Future"
        )
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RustTarget {
    Caribou,
    HashLink,
    Rayzor,
}

fn exported(_: RustTarget, _: &syn::Ident, _: &syn::Ident) -> TokenStream {
    // Rayzor receives separate ABI-normalising wrappers. The typed model
    // functions stay ordinary associated functions on every target.
    TokenStream::new()
}

fn stored_value(
    ty: &Type,
    resources: &HashSet<String>,
    records: &HashSet<String>,
    target: RustTarget,
) -> Result<(TokenStream, TokenStream, TokenStream), String> {
    if let Some(enumeration) = generic(ty, "Enum") {
        return Ok((
            quote!(i32),
            quote!(Enum<#enumeration>),
            quote!(value.get().native()),
        ));
    }
    if scalar(ty) {
        if type_name(ty).as_deref() == Some("Text") {
            let rooted = if target == RustTarget::Caribou {
                quote!(caribou_abi::Rooted)
            } else {
                quote!(Rooted)
            };
            return Ok((
                quote!(#rooted<Text>),
                quote!(Text),
                quote!(#rooted::new(value)),
            ));
        }
        if type_name(ty).as_deref() == Some("Buffer") {
            let rooted = if target == RustTarget::Caribou {
                quote!(caribou_abi::Rooted)
            } else {
                quote!(Rooted)
            };
            return Ok((
                quote!(#rooted<Buffer>),
                quote!(Buffer),
                quote!(#rooted::new(value)),
            ));
        }
        return Ok((quote!(#ty), quote!(#ty), quote!(value)));
    }
    let name = type_name(ty).ok_or("record fields need named types")?;
    let ident = ident(&name)?;
    if resources.contains(&name) {
        Ok((quote!(i32), quote!(&#ident), quote!(value.handle)))
    } else if records.contains(&name) {
        Ok((quote!(#ident), quote!(&#ident), quote!(value.clone())))
    } else {
        Err(format!("unsupported record field type {name}"))
    }
}

/// Emit a self-contained set of resource wrappers, schemas and one plugin
/// table. Backends implement the selected functions with integer handles;
/// the generated ABI uses typed native objects, enums, Text and Buffer.
pub fn generate_caribou(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
) -> Result<String, String> {
    generate_parts(
        namespace,
        &declaration.into(),
        webidl,
        RustTarget::Caribou,
        &HashSet::new(),
    )
    .map(|(code, _, _)| code)
}

/// The native library a binding's symbols live in: the HashLink library its
/// Haxe surface loads (`<name>.hdll`, or `<name>.wasm` under Ash), the tag
/// of its records' HashLink abstracts, and the prefix of its Rayzor symbols.
/// Two bindings one program loads each need their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Library<'a>(pub &'a str);

impl<'a> Library<'a> {
    /// The library of the functions that take no library.
    pub const XIDL: Library<'static> = Library("xidl");

    fn name(self) -> Result<&'a str, String> {
        let valid = self.0.starts_with(|c: char| c.is_ascii_alphabetic())
            && self
                .0
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!(
                "library name {:?} is not [A-Za-z][A-Za-z0-9_]*",
                self.0
            ));
        }
        Ok(self.0)
    }

    /// Emit the runtime-neutral model and HashLink primitive resolvers used
    /// by the plugin. Resources cross as integer handles, records as
    /// GC-finalized native abstracts, and Promise results as Ash Future
    /// carriers.
    pub fn generate_hashlink(
        self,
        namespace: &str,
        declaration: impl Into<Declaration>,
        webidl: &str,
    ) -> Result<String, String> {
        let library = self.name()?;
        let (model, _, plugin) = generate_parts(
            namespace,
            &declaration.into(),
            webidl,
            RustTarget::HashLink,
            &HashSet::new(),
        )?;
        let registration = hashlink_registration(&model, &plugin, library)?;
        Ok(format!("{model} {registration}"))
    }

    /// Generate Rayzor bindings, letting its adapter own the resource
    /// wrappers named in `adapter_resources`. This is how runtime extensions
    /// attach metadata to a handle without creating a second language object
    /// for the same resource.
    pub fn generate_rayzor(
        self,
        namespace: &str,
        declaration: impl Into<Declaration>,
        webidl: &str,
        adapter_resources: &[&str],
    ) -> Result<String, String> {
        self.generate_rayzor_in(namespace, namespace, declaration, webidl, adapter_resources)
    }

    /// As `generate_rayzor`, naming each class in the method table by the
    /// Haxe package its externs are in (`rayzor.gpu`), as `haxe` is given
    /// it. Rayzor matches a method to its extern by that name.
    pub fn generate_rayzor_in(
        self,
        namespace: &str,
        package: &str,
        declaration: impl Into<Declaration>,
        webidl: &str,
        adapter_resources: &[&str],
    ) -> Result<String, String> {
        let library = self.name()?;
        let adapter_resources = adapter_resources
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let (model, _, _) = generate_parts(
            namespace,
            &declaration.into(),
            webidl,
            RustTarget::Rayzor,
            &adapter_resources,
        )?;
        let registration = rayzor_registration(package, &model, library)?;
        Ok(format!("{model} {registration}"))
    }

    /// The conventional Haxe surface for one runtime, its natives in this
    /// library.
    pub fn haxe(
        self,
        namespace: &str,
        declaration: impl Into<Declaration>,
        webidl: &str,
        runtime: haxe::Runtime,
    ) -> Result<Vec<haxe::File>, String> {
        haxe::generate_in(self.name()?, namespace, declaration.into(), webidl, runtime)
    }
}

/// Emit the runtime-neutral model and exported C symbols used by Rayzor's
/// native package. The adapter supplies Text, Buffer, roots, futures, errors,
/// and the generic Enum carrier; xidl supplies the object model and backend.
pub fn generate_rayzor(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
) -> Result<String, String> {
    generate_rayzor_with_resources(namespace, declaration, webidl, &[])
}

/// `Library::generate_hashlink` in the `xidl` library.
pub fn generate_hashlink(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
) -> Result<String, String> {
    Library::XIDL.generate_hashlink(namespace, declaration, webidl)
}

/// `Library::generate_rayzor_in` in the `xidl` library.
pub fn generate_rayzor_in(
    namespace: &str,
    package: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
    adapter_resources: &[&str],
) -> Result<String, String> {
    Library::XIDL.generate_rayzor_in(namespace, package, declaration, webidl, adapter_resources)
}

/// `Library::generate_rayzor` in the `xidl` library.
pub fn generate_rayzor_with_resources(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
    adapter_resources: &[&str],
) -> Result<String, String> {
    Library::XIDL.generate_rayzor(namespace, declaration, webidl, adapter_resources)
}

/// Compatibility spelling for existing Caribou build scripts.
pub fn generate(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
) -> Result<String, String> {
    generate_caribou(namespace, declaration, webidl)
}

/// A backend function the generated members call: its argument and
/// result types as the backend takes them, and what a caller gets back
/// when it fails.
struct BackendFn {
    name: syn::Ident,
    params: Vec<TokenStream>,
    ret: TokenStream,
    fallback: TokenStream,
    /// The WebIDL member a method is tagged with, from which a web backend
    /// generates the function when its adapter does not write it.
    member: Option<Member>,
}

/// A method tagged `#[idl("Interface.member")]`, as its declaration has it.
struct Member {
    /// `GPUTexture.width`.
    source: String,
    class: syn::Ident,
    /// Each argument's declared type, the receiver first.
    args: Vec<Type>,
    returns: Option<Type>,
}

fn rayzor_abi_type(ty: &Type, result: bool) -> Result<u8, String> {
    if matches!(ty, Type::Reference(_)) {
        return Ok(3);
    }
    if let Type::Tuple(tuple) = ty
        && tuple.elems.is_empty()
    {
        return Ok(0);
    }
    if generic(ty, "Box").is_some() || generic(ty, "Future").is_some() {
        return Ok(3);
    }
    if generic(ty, "Enum").is_some() {
        return Ok(1);
    }
    match type_name(ty).as_deref() {
        Some("bool") => Ok(4),
        Some("f32" | "f64") => Ok(2),
        Some("i32" | "u32" | "i64" | "u64") => Ok(1),
        Some("Text" | "Buffer" | "BufferMut" | "Future") => Ok(3),
        Some(name) if result => Err(format!("unsupported Rayzor result type {name}")),
        Some(name) => Err(format!("unsupported Rayzor parameter type {name}")),
        None => Err("unsupported composite type in Rayzor ABI".into()),
    }
}

fn rayzor_rust_type(ty: &Type, result: bool) -> Result<TokenStream, String> {
    Ok(match rayzor_abi_type(ty, result)? {
        0 => quote!(()),
        1 => quote!(i64),
        2 => quote!(f64),
        3 => quote!(*mut u8),
        4 => quote!(bool),
        _ => unreachable!(),
    })
}

fn rayzor_argument(name: &syn::Ident, ty: &Type) -> Result<TokenStream, String> {
    if let Type::Reference(reference) = ty {
        let inner = &reference.elem;
        return Ok(if reference.mutability.is_some() {
            quote!(unsafe { &mut *(#name as *mut #inner) })
        } else {
            quote!(unsafe { &*(#name as *const #inner) })
        });
    }
    if generic(ty, "Box").is_some() {
        return Err("Box parameters are not supported by the Rayzor ABI".into());
    }
    if generic(ty, "Enum").is_some() {
        return Ok(quote!(unsafe { std::mem::transmute::<i64, #ty>(#name) }));
    }
    if generic(ty, "Future").is_some()
        || matches!(
            type_name(ty).as_deref(),
            Some("Text" | "Buffer" | "BufferMut" | "Future")
        )
    {
        return Ok(quote!(unsafe { std::mem::transmute::<*mut u8, #ty>(#name) }));
    }
    Ok(match type_name(ty).as_deref() {
        Some("i32" | "u32" | "i64" | "u64") => quote!(#name as #ty),
        Some("f32") => quote!(#name as f32),
        Some("f64" | "bool") => quote!(#name),
        Some(name) => return Err(format!("unsupported Rayzor argument type {name}")),
        None => return Err("unsupported composite type in Rayzor ABI".into()),
    })
}

fn rayzor_return(
    call: TokenStream,
    output: &ReturnType,
) -> Result<(TokenStream, TokenStream), String> {
    let ReturnType::Type(_, ty) = output else {
        return Ok((quote!(), quote!({ #call; })));
    };
    let abi = rayzor_rust_type(ty, true)?;
    let convert = if generic(ty, "Box").is_some() {
        quote!(Box::into_raw(value) as *mut u8)
    } else if generic(ty, "Enum").is_some() {
        quote!(unsafe { std::mem::transmute::<#ty, i64>(value) })
    } else if generic(ty, "Future").is_some()
        || matches!(
            type_name(ty).as_deref(),
            Some("Text" | "Buffer" | "BufferMut" | "Future")
        )
    {
        quote!(unsafe { std::mem::transmute::<#ty, *mut u8>(value) })
    } else {
        match type_name(ty).as_deref() {
            Some("i32" | "u32" | "i64" | "u64") => quote!(value as i64),
            Some("f32" | "f64") => quote!(value as f64),
            Some("bool") => quote!(value),
            Some(name) => return Err(format!("unsupported Rayzor result type {name}")),
            None => return Err("unsupported composite result in Rayzor ABI".into()),
        }
    };
    Ok((abi, quote!({ let value = #call; #convert })))
}

/// Describe every generated C export to Rayzor's compiler and return the same
/// function pointers to its runtime linker. This is derived from the emitted
/// model so the externs, method table and actual symbols cannot drift apart.
fn rayzor_registration(package: &str, model: &str, library: &str) -> Result<String, String> {
    let file = syn::parse_file(model).map_err(error)?;
    let mut descriptors = TokenStream::new();
    let mut symbols = TokenStream::new();
    let mut wrappers = TokenStream::new();
    let mut count = 0usize;
    for item in file.items {
        let Item::Impl(item) = item else { continue };
        let Type::Path(class_path) = &*item.self_ty else {
            continue;
        };
        let Some(class) = class_path.path.get_ident() else {
            continue;
        };
        for member in item.items {
            let syn::ImplItem::Fn(method) = member else {
                continue;
            };
            if method
                .sig
                .abi
                .as_ref()
                .and_then(|abi| abi.name.as_ref())
                .is_none_or(|name| name.value() != "C")
            {
                continue;
            }
            let symbol = format!(
                "{library}_{}_{}",
                haxe::snake(&class.to_string()),
                haxe::snake(&method.sig.ident.unraw().to_string())
            );
            let method_name = method.sig.ident.unraw().to_string();
            let class_name = format!("{}::{class}", package.replace('.', "::"));
            let mut params = Vec::new();
            let mut wrapper_params = Vec::new();
            let mut wrapper_args = Vec::new();
            let mut instance = false;
            for (at, arg) in method.sig.inputs.iter().enumerate() {
                let FnArg::Typed(arg) = arg else {
                    return Err("generated Rayzor functions use typed parameters".into());
                };
                if at == 0 && matches!(&*arg.pat, syn::Pat::Ident(p) if p.ident == "this") {
                    instance = true;
                }
                params.push(rayzor_abi_type(&arg.ty, false)?);
                let name = quote::format_ident!("a{at}");
                let ty = rayzor_rust_type(&arg.ty, false)?;
                wrapper_params.push(quote!(#name: #ty));
                wrapper_args.push(rayzor_argument(&name, &arg.ty)?);
            }
            if params.len() > 16 {
                return Err(format!(
                    "{class}.{method_name} has {} ABI parameters; Rayzor supports 16",
                    params.len()
                ));
            }
            let ret = match &method.sig.output {
                ReturnType::Default => 0,
                ReturnType::Type(_, ty) => rayzor_abi_type(ty, true)?,
            };
            let param_count = u8::try_from(params.len()).map_err(error)?;
            let is_static = u8::from(!instance);
            let mut padded = params;
            padded.resize(16, 0);
            let function = &method.sig.ident;
            let wrapper = quote::format_ident!("__{symbol}");
            let call = quote!(#class::#function(#(#wrapper_args),*));
            let (wrapper_ret, wrapper_body) = rayzor_return(call, &method.sig.output)?;
            let wrapper_output = if wrapper_ret.is_empty() {
                quote!()
            } else {
                quote!(-> #wrapper_ret)
            };
            wrappers.extend(quote! {
                #[unsafe(export_name = #symbol)]
                pub extern "C" fn #wrapper(#(#wrapper_params),*) #wrapper_output #wrapper_body
            });
            descriptors.extend(quote! {
                rayzor_plugin::NativeMethodDesc {
                    symbol_name: #symbol.as_ptr(),
                    symbol_name_len: #symbol.len(),
                    class_name: #class_name.as_ptr(),
                    class_name_len: #class_name.len(),
                    method_name: #method_name.as_ptr(),
                    method_name_len: #method_name.len(),
                    is_static: #is_static,
                    param_count: #param_count,
                    return_type: #ret,
                    param_types: [#(#padded),*],
                },
            });
            symbols.extend(quote!((#symbol, #wrapper as *const u8),));
            count += 1;
        }
    }
    if count == 0 {
        return Err("Rayzor generation produced no exported methods".into());
    }
    Ok(quote! {
        #wrappers
        pub static XIDL_METHODS: &[rayzor_plugin::NativeMethodDesc] = &[#descriptors];
        pub fn xidl_runtime_symbols() -> Vec<(&'static str, *const u8)> {
            vec![#symbols]
        }
    }
    .to_string())
}

fn hashlink_signature(
    ty: &Type,
    result: bool,
    plugin: &convert::Plugin,
    library: &str,
) -> Result<String, String> {
    if let Type::Reference(reference) = ty {
        let name = type_name(&reference.elem).ok_or("HashLink references need named types")?;
        if plugin.resources.contains(&name) {
            return Ok("i".into());
        }
        if plugin.records.iter().any(|record| record.class == name) {
            return Ok(format!("X{library}_{name}_"));
        }
        return Err(format!("unsupported HashLink reference {name}"));
    }
    if let Type::Tuple(tuple) = ty
        && tuple.elems.is_empty()
    {
        return Ok("v".into());
    }
    if let Some(inner) = generic(ty, "Box") {
        let name = type_name(&inner).ok_or("HashLink boxes need named types")?;
        return if plugin.resources.contains(&name) {
            Ok("i".into())
        } else if plugin.records.iter().any(|record| record.class == name) {
            Ok(format!("X{library}_{name}_"))
        } else {
            Err(format!("unsupported HashLink box {name}"))
        };
    }
    if generic(ty, "Future").is_some() {
        return Ok("Xash_future_".into());
    }
    if generic(ty, "Enum").is_some() {
        return Ok("i".into());
    }
    match type_name(ty).as_deref() {
        Some("bool") => Ok("b".into()),
        // Haxe's Float is a double, so f32 crosses as one, as on Rayzor.
        Some("f32" | "f64") => Ok("d".into()),
        Some("i32" | "u32") => Ok("i".into()),
        Some("i64" | "u64") => Ok("l".into()),
        Some("Text") if result => Ok("B".into()),
        Some("Buffer") if result => Ok(format!("X{library}_buffer_result_")),
        // A String is its bytes then its length; a haxe.io.Bytes is its
        // length then its bytes.
        Some("Text") => Ok("OBi_".into()),
        Some("Buffer" | "BufferMut") => Ok("OiB_".into()),
        Some(name) => Err(format!("unsupported HashLink ABI type {name}")),
        None => Err("unsupported composite HashLink ABI type".into()),
    }
}

fn hashlink_argument(
    name: &syn::Ident,
    ty: &Type,
    plugin: &convert::Plugin,
) -> Result<(TokenStream, TokenStream), String> {
    if let Type::Reference(reference) = ty {
        let target = type_name(&reference.elem).ok_or("HashLink references need named types")?;
        let inner = &reference.elem;
        if plugin.resources.contains(&target) {
            return Ok((
                if reference.mutability.is_some() {
                    quote!(mut #name: i32)
                } else {
                    quote!(#name: i32)
                },
                if reference.mutability.is_some() {
                    quote!(unsafe { &mut *(&mut #name as *mut i32).cast::<#inner>() })
                } else {
                    quote!(unsafe { &*(&#name as *const i32).cast::<#inner>() })
                },
            ));
        }
        if plugin.records.iter().any(|record| record.class == target) {
            return Ok((
                quote!(#name: *mut runtime::Managed<#inner>),
                if reference.mutability.is_some() {
                    quote!(unsafe { runtime::managed_mut(#name) })
                } else {
                    quote!(unsafe { runtime::managed_ref(#name) })
                },
            ));
        }
    }
    if generic(ty, "Enum").is_some() {
        return Ok((quote!(#name: i32), quote!(Enum::from_native(#name))));
    }
    match type_name(ty).as_deref() {
        Some("Text") => Ok((
            quote!(#name: *mut hl_abi::vstring),
            quote!(unsafe { Text::from_hl(#name) }),
        )),
        Some("Buffer") => Ok((
            quote!(#name: *mut runtime::HlBytes),
            quote!(unsafe { Buffer::from_hl(#name) }),
        )),
        Some("BufferMut") => Ok((
            quote!(#name: *mut runtime::HlBytes),
            quote!(unsafe { BufferMut::from_hl(#name) }),
        )),
        Some("f32") => Ok((quote!(#name: f64), quote!(#name as f32))),
        Some("i32" | "u32" | "i64" | "u64" | "f64" | "bool") => {
            Ok((quote!(#name: #ty), quote!(#name)))
        }
        Some(target) => Err(format!("unsupported HashLink argument {target}")),
        None => Err("unsupported composite HashLink argument".into()),
    }
}

fn hashlink_return(
    call: TokenStream,
    output: &ReturnType,
    plugin: &convert::Plugin,
) -> Result<(TokenStream, TokenStream), String> {
    let ReturnType::Type(_, ty) = output else {
        return Ok((quote!(), quote!({ #call; })));
    };
    if let Some(inner) = generic(ty, "Box") {
        let name = type_name(&inner).ok_or("HashLink boxes need named types")?;
        if plugin.resources.contains(&name) {
            return Ok((quote!(i32), quote!({ let value = #call; value.handle })));
        }
        if plugin.records.iter().any(|record| record.class == name) {
            return Ok((
                quote!(*mut runtime::Managed<#inner>),
                quote!({ let value = #call; runtime::managed_new(*value) }),
            ));
        }
    }
    if generic(ty, "Enum").is_some() {
        return Ok((
            quote!(i32),
            quote!({ let value = #call; value.get().native() }),
        ));
    }
    if let Some(inner) = generic(ty, "Future") {
        return Ok((
            quote!(*mut ash_future_abi::AshFuture),
            quote!({ let value: Future<#inner> = #call; value.as_ptr() }),
        ));
    }
    match type_name(ty).as_deref() {
        Some("Text") => Ok((
            quote!(*mut hl_abi::vbyte),
            quote!({ let value = #call; value.into_ucs2() }),
        )),
        Some("Buffer") => Ok((
            quote!(*mut runtime::Managed<Buffer>),
            quote!({ let value = #call; runtime::managed_new(value) }),
        )),
        Some("f32") => Ok((quote!(f64), quote!({ let value = #call; value as f64 }))),
        Some("i32" | "u32" | "i64" | "u64" | "f64" | "bool") => {
            Ok((quote!(#ty), quote!({ #call })))
        }
        Some(target) => Err(format!("unsupported HashLink result {target}")),
        None => Err("unsupported composite HashLink result".into()),
    }
}

/// Derive every `DEFINE_PRIM` resolver from the generated typed model so its
/// signature and the Haxe surface cannot drift apart.
fn hashlink_registration(
    model: &str,
    plugin: &convert::Plugin,
    library: &str,
) -> Result<String, String> {
    let file = syn::parse_file(model).map_err(error)?;
    let mut wrappers = TokenStream::new();
    let mut count = 0usize;
    let mut returns_bytes = false;
    for item in file.items {
        let Item::Impl(item) = item else { continue };
        let Type::Path(class_path) = &*item.self_ty else {
            continue;
        };
        let Some(class) = class_path.path.get_ident() else {
            continue;
        };
        for member in item.items {
            let syn::ImplItem::Fn(method) = member else {
                continue;
            };
            if method.sig.abi.as_ref().is_none() {
                continue;
            }
            returns_bytes |= matches!(&method.sig.output,
                ReturnType::Type(_, ty) if type_name(ty).as_deref() == Some("Buffer"));
            let method_name = method.sig.ident.unraw().to_string();
            let native_name = format!(
                "{}_{}",
                haxe::snake(&class.to_string()),
                haxe::snake(&method_name),
            );
            let function = &method.sig.ident;
            let wrapper = quote::format_ident!("__hl_{native_name}");
            let resolver = quote::format_ident!("hlp_{native_name}");
            let mut params = Vec::new();
            let mut args = Vec::new();
            let mut signature = String::from("P");
            for (at, arg) in method.sig.inputs.iter().enumerate() {
                let FnArg::Typed(arg) = arg else {
                    return Err("generated HashLink functions use typed parameters".into());
                };
                signature.push_str(&hashlink_signature(&arg.ty, false, plugin, library)?);
                let name = quote::format_ident!("a{at}");
                let (parameter, converted) = hashlink_argument(&name, &arg.ty, plugin)?;
                params.push(parameter);
                args.push(converted);
            }
            signature.push('_');
            match &method.sig.output {
                ReturnType::Default => signature.push('v'),
                ReturnType::Type(_, ty) => {
                    signature.push_str(&hashlink_signature(ty, true, plugin, library)?)
                }
            }
            let call = quote!(#class::#function(#(#args),*));
            let (ret, body) = hashlink_return(call, &method.sig.output, plugin)?;
            let output = if ret.is_empty() {
                quote!()
            } else {
                quote!(-> #ret)
            };
            wrappers.extend(quote! {
                #[unsafe(no_mangle)]
                pub unsafe extern "C" fn #wrapper(#(#params),*) #output #body
                hl_abi::define_prim!(#resolver, #wrapper, #signature);
            });
            count += 1;
        }
    }
    if count == 0 {
        return Err("HashLink generation produced no primitives".into());
    }
    if returns_bytes {
        // What the Haxe surface's XidlBytes copies a Buffer result out with.
        let len = format!("PX{library}_buffer_result__i");
        let copy = format!("PX{library}_buffer_result_OiB__v");
        wrappers.extend(quote! {
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn __hl_buffer_result_len(
                value: *mut runtime::Managed<Buffer>,
            ) -> i32 {
                if value.is_null() {
                    return 0;
                }
                unsafe { runtime::managed_ref(value) }.len() as i32
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn __hl_buffer_result_copy(
                value: *mut runtime::Managed<Buffer>,
                out: *mut runtime::HlBytes,
            ) {
                if value.is_null() || out.is_null() {
                    return;
                }
                let value = unsafe { runtime::managed_ref(value) };
                let out = unsafe { &*out };
                let len = value.len().min(out.length.max(0) as usize);
                unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), out.b, len) };
            }
            hl_abi::define_prim!(hlp_buffer_result_len, __hl_buffer_result_len, #len);
            hl_abi::define_prim!(hlp_buffer_result_copy, __hl_buffer_result_copy, #copy);
        });
    }
    Ok(wrappers.to_string())
}

fn generate_parts(
    namespace: &str,
    declaration: &Declaration,
    webidl: &str,
    target: RustTarget,
    adapter_resources: &HashSet<String>,
) -> Result<(String, Vec<BackendFn>, convert::Plugin), String> {
    ident(namespace)?;
    let mut backend_fns: Vec<BackendFn> = Vec::new();
    let mut described = Vec::new();
    let desc_content = declaration.text()?;

    let file = syn::parse_file(&desc_content).map_err(error);
    if let Err(e) = &file {
        return Err(format!("declaration: {e}"));
    }
    let idl = tokens(webidl)?;
    let aliases = typedefs(&idl);

    let classes: HashSet<_> = if let Ok(file) = &file {
        file.items
            .iter()
            .filter_map(|i| match i {
                Item::Trait(t) => Some(t.ident.to_string()),
                Item::Struct(s) => Some(s.ident.to_string()),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    let resources: HashSet<_> = if let Ok(file) = &file {
        file.items
            .iter()
            .filter_map(|i| match i {
                Item::Trait(t) => Some(t.ident.to_string()),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    let records: HashSet<_> = if let Ok(file) = &file {
        file.items
            .iter()
            .filter_map(|i| match i {
                Item::Struct(s) => Some(s.ident.to_string()),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    // An enum whose variants carry a value declares a union: a record field
    // of that type takes one setter per alternative instead of a dynamic
    // value. Each variant holds exactly one declared type.
    let mut unions: HashMap<String, Vec<(syn::Ident, Type)>> = HashMap::new();
    let mut union_extensions: HashSet<(String, String)> = HashSet::new();
    // An enum with a variant of named fields declares variants: a value of
    // one of several shapes, which a resource method returns.
    let mut variants: HashMap<String, Variants> = HashMap::new();
    if let Ok(file) = &file {
        let kinds: HashSet<String> = file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Enum(e)
                    if e.variants
                        .iter()
                        .any(|v| matches!(v.fields, syn::Fields::Named(_))) =>
                {
                    Some(e.ident.to_string())
                }
                _ => None,
            })
            .collect();
        for item in &file.items {
            let Item::Enum(e) = item else { continue };
            if kinds.contains(&e.ident.to_string()) {
                variants.insert(e.ident.to_string(), declared_variants(e, &kinds)?);
            }
        }
        acyclic(&variants)?;
        for item in &file.items {
            let Item::Enum(e) = item else { continue };
            if variants.contains_key(&e.ident.to_string())
                || e.variants
                    .iter()
                    .all(|v| matches!(v.fields, syn::Fields::Unit))
            {
                continue;
            }
            let mut alternatives = Vec::new();
            for v in &e.variants {
                let syn::Fields::Unnamed(fields) = &v.fields else {
                    return Err(format!(
                        "union {}::{} needs one unnamed type",
                        e.ident, v.ident
                    ));
                };
                if fields.unnamed.len() != 1 || v.discriminant.is_some() {
                    return Err(format!(
                        "union {}::{} needs one unnamed type",
                        e.ident, v.ident
                    ));
                }
                if extension(&v.attrs)? {
                    union_extensions.insert((e.ident.to_string(), v.ident.to_string()));
                }
                alternatives.push((v.ident.clone(), fields.unnamed[0].ty.clone()));
            }
            unions.insert(e.ident.to_string(), alternatives);
        }
    }

    let enums: HashSet<_> = if let Ok(file) = &file {
        file.items
            .iter()
            .filter_map(|i| match i {
                Item::Enum(e)
                    if !unions.contains_key(&e.ident.to_string())
                        && !variants.contains_key(&e.ident.to_string()) =>
                {
                    Some(e.ident.to_string())
                }
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    let mut idl_types = HashMap::new();
    if let Ok(file) = &file {
        for item in &file.items {
            let (attrs, ty): (&[syn::Attribute], Type) = match item {
                Item::Enum(item) if variants.contains_key(&item.ident.to_string()) => {
                    if idl_name(&item.attrs)?.is_some() {
                        return Err(format!("variants {} are not imported", item.ident));
                    }
                    continue;
                }
                Item::Enum(item) if unions.contains_key(&item.ident.to_string()) => {
                    let local = &item.ident;
                    (item.attrs.as_slice(), syn::parse_quote!(#local))
                }
                Item::Enum(item) => {
                    let local = &item.ident;
                    (item.attrs.as_slice(), syn::parse_quote!(Enum<#local>))
                }
                Item::Struct(item) => {
                    let local = &item.ident;
                    (item.attrs.as_slice(), syn::parse_quote!(#local))
                }
                Item::Trait(item) => {
                    let local = &item.ident;
                    (item.attrs.as_slice(), syn::parse_quote!(#local))
                }
                _ => continue,
            };
            if let Some(source) = idl_name(attrs)?
                && idl_types.insert(source.clone(), ty).is_some()
            {
                return Err(format!("WebIDL type {source} is imported more than once"));
            }
        }
    }
    // Resources that import no WebIDL interface, which a tagged method's
    // result may stand for.
    let imported: HashSet<String> = idl_types.values().filter_map(type_name).collect();
    let untagged: HashSet<String> = resources
        .iter()
        .filter(|r| !imported.contains(*r))
        .cloned()
        .collect();
    let mut names = HashSet::new();
    let mut output = TokenStream::new();
    let mut exports = TokenStream::new();
    if let Ok(file) = &file {
        for item in &file.items {
            let name = match &item {
                Item::Enum(e) => &e.ident,
                Item::Trait(t) => &t.ident,
                Item::Struct(s) => &s.ident,
                Item::Mod(m) => &m.ident,
                _ => {
                    return Err(
                        "declarations support enums, records, resource traits and constant modules"
                            .into(),
                    );
                }
            };
            if !names.insert(name.to_string()) {
                return Err(format!("duplicate export {name}"));
            }
            match item {
                Item::Enum(e) if variants.contains_key(&e.ident.to_string()) => {
                    let name = &e.ident;
                    let schema = format!("{namespace}.{name}");
                    let declared = &variants[&name.to_string()];
                    let mut shapes = Vec::new();
                    let mut indices = Vec::new();
                    for (at, (variant, fields)) in declared.iter().enumerate() {
                        let at =
                            proc_macro2::Literal::i32_unsuffixed(i32::try_from(at).map_err(error)?);
                        if fields.is_empty() {
                            shapes.push(quote!(#variant));
                            indices.push(quote!(Self::#variant => #at));
                            continue;
                        }
                        let mut stored = Vec::new();
                        for (field, ty) in fields {
                            if let Some(enumeration) = generic(ty, "Enum")
                                && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                            {
                                return Err(format!("unknown enum in {name}::{variant}.{field}"));
                            }
                            let ty = variant_storage(ty);
                            stored.push(quote!(#field: #ty));
                        }
                        shapes.push(quote!(#variant { #(#stored),* }));
                        indices.push(quote!(Self::#variant { .. } => #at));
                    }
                    let derive = if target == RustTarget::Caribou {
                        quote! {
                            #[derive(Debug, Clone, PartialEq, caribou_abi::PluginEnum)]
                            #[caribou(name = #schema)]
                        }
                    } else {
                        quote!(#[derive(Debug, Clone, PartialEq)])
                    };
                    // The first variant, its fields at their defaults: what a
                    // failed call returns.
                    let (first, fields) = &declared[0];
                    let default = if fields.is_empty() {
                        quote!(Self::#first)
                    } else {
                        let fields = fields.iter().map(|(field, _)| field);
                        quote!(Self::#first { #(#fields: Default::default()),* })
                    };
                    output.extend(quote! {
                        #derive
                        pub enum #name { #(#shapes),* }
                        impl Default for #name {
                            fn default() -> Self { #default }
                        }
                        impl #name {
                            /// The variant's position in the declaration.
                            #[allow(dead_code)]
                            pub fn variant(&self) -> i32 { match self { #(#indices),* } }
                        }
                    });
                    exports.extend(quote!(enum #name;));
                }
                Item::Enum(e) if unions.contains_key(&e.ident.to_string()) => {
                    let name = &e.ident;
                    let alternatives = &unions[&name.to_string()];
                    if let Some(source) = idl_name(&e.attrs)? {
                        // A declared subset of the WebIDL union: each variant
                        // must be one of its alternatives.
                        let union = aliases
                            .get(&source)
                            .map(|alias| strip_attributes(alias))
                            .and_then(union_alternatives)
                            .ok_or_else(|| format!("{source} is not a WebIDL union typedef"))?;
                        let mapped: Vec<String> = union
                            .into_iter()
                            .filter_map(|alternative| {
                                idl_type(alternative, &aliases, &idl_types, &mut HashSet::new())
                                    .ok()
                            })
                            .map(|ty| quote!(#ty).to_string())
                            .collect();
                        for (variant, ty) in alternatives {
                            let extra = (name.to_string(), variant.to_string());
                            if !union_extensions.contains(&extra)
                                && !mapped.contains(&quote!(#ty).to_string())
                            {
                                return Err(format!(
                                    "{name}::{variant} is not an alternative of {source}"
                                ));
                            }
                        }
                    }
                    let mut stored_variants = Vec::new();
                    for (variant, ty) in alternatives {
                        if let Some(enumeration) = generic(ty, "Enum")
                            && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                        {
                            return Err(format!("unknown enum in {name}::{variant}"));
                        }
                        let (stored, _, _) = stored_value(ty, &resources, &records, target)?;
                        stored_variants.push(quote!(#variant(#stored)));
                    }
                    output.extend(quote! {
                        #[derive(Clone)]
                        pub enum #name { #(#stored_variants),* }
                    });
                }
                Item::Enum(e) => {
                    let name = &e.ident;
                    let schema = format!("{namespace}.{name}");
                    let source = idl_name(&e.attrs)?;
                    // Native codes are evaluated here, so the generated code
                    // matches on plain integer literals.
                    // An imported enum may add `#[extension]` values after the
                    // WebIDL ones; their codes continue from the last.
                    let all_extensions = e
                        .variants
                        .iter()
                        .map(|v| extension(&v.attrs))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .all(|extra| extra);
                    let variants: Vec<(syn::Ident, i32)> = if let Some(source) =
                        source.filter(|_| all_extensions)
                    {
                        let mut values = enum_values(&idl, &source)
                            .or_else(|_| readonly_attribute_names(&idl, &source))?
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let code = i32::try_from(i).map_err(error)?;
                                Ok((ident(&pascal(&v))?, code))
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        for v in &e.variants {
                            if !matches!(v.fields, syn::Fields::Unit) || v.discriminant.is_some() {
                                return Err(format!(
                                    "{name}.{} extends a WebIDL enum: no fields or value",
                                    v.ident
                                ));
                            }
                            let code = i32::try_from(values.len()).map_err(error)?;
                            values.push((v.ident.clone(), code));
                        }
                        values
                    } else {
                        let mut next = 0i32;
                        let mut values = Vec::new();
                        for v in &e.variants {
                            if !matches!(v.fields, syn::Fields::Unit) {
                                return Err("native enums must be fieldless".into());
                            }
                            if extension(&v.attrs)? {
                                return Err(format!(
                                    "{name}.{}: #[extension] needs an imported enum whose other values come from WebIDL",
                                    v.ident
                                ));
                            }
                            let value = match &v.discriminant {
                                Some((_, expr)) => discriminant(expr).ok_or_else(|| {
                                    format!("{name}.{} needs an integer literal", v.ident)
                                })?,
                                None => next,
                            };
                            next = value
                                .checked_add(1)
                                .ok_or_else(|| format!("{name} overflows i32"))?;
                            values.push((v.ident.clone(), value));
                        }
                        values
                    };
                    if variants.is_empty() {
                        return Err(format!("empty enum {name}"));
                    }
                    let mut seen = HashSet::new();
                    for (v, _) in &variants {
                        if !seen.insert(v.to_string()) {
                            return Err(format!("duplicate variant {name}.{v}"));
                        }
                    }
                    let ids: Vec<_> = variants.iter().map(|(v, _)| v).collect();
                    let values: Vec<_> = variants
                        .iter()
                        .map(|(_, value)| proc_macro2::Literal::i32_unsuffixed(*value))
                        .collect();
                    let (first, rest) = ids.split_first().expect("a non-empty enum");
                    let derive = if target == RustTarget::Caribou {
                        quote! {
                            #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, caribou_abi::PluginEnum)]
                            #[caribou(name = #schema)]
                        }
                    } else {
                        quote!(#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)])
                    };
                    let native_enum = if target != RustTarget::Caribou {
                        quote! {
                            impl NativeEnum for #name {
                                fn native(self) -> i32 { #name::native(self) }
                                fn from_native(value: i32) -> Option<Self> {
                                    #name::from_native(value)
                                }
                            }
                        }
                    } else {
                        TokenStream::new()
                    };
                    output.extend(quote! {
                        #derive
                        pub enum #name { #[default] #first, #(#rest),* }
                        impl #name {
                            pub fn native(self) -> i32 { match self { #(Self::#ids => #values),* } }
                            pub fn from_native(value: i32) -> Option<Self> {
                                match value {
                                    #(#values => Some(Self::#ids),)*
                                    _ => None,
                                }
                            }
                        }
                        #native_enum
                    });
                    exports.extend(quote!(enum #name;));
                }
                Item::Mod(m) => {
                    let name = &m.ident;
                    let mut constants: Vec<(syn::Ident, syn::LitInt)> = Vec::new();
                    if let Some(source) = idl_name(&m.attrs)? {
                        let body = body(&idl, "namespace", &source)?;
                        for statement in body.split(|s| s == ";").filter(|s| !s.is_empty()) {
                            if statement.len() != 5
                                || statement[0] != "const"
                                || statement[3] != "="
                            {
                                return Err(format!("unsupported constant in {source}"));
                            }
                            constants.push((
                                ident(&statement[2])?,
                                syn::parse_str(&statement[4]).map_err(error)?,
                            ));
                        }
                    }
                    // Constants the backend has beyond the WebIDL namespace, or a
                    // namespace of its own: `const NAME: i32 = value;`.
                    for item in m
                        .content
                        .as_ref()
                        .map(|(_, items)| items.as_slice())
                        .unwrap_or(&[])
                    {
                        let syn::Item::Const(constant) = item else {
                            return Err(format!("{name} holds only i32 constants"));
                        };
                        let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Int(value),
                            ..
                        }) = &*constant.expr
                        else {
                            return Err(format!(
                                "{name}::{} needs an integer literal",
                                constant.ident
                            ));
                        };
                        constants.push((constant.ident.clone(), value.clone()));
                    }
                    if constants.is_empty() {
                        return Err(format!("{name} declares no constants"));
                    }
                    let mut methods = TokenStream::new();
                    let mut signatures = TokenStream::new();
                    let mut seen = HashSet::new();
                    for (field, value) in constants {
                        if !seen.insert(field.to_string()) {
                            return Err(format!("duplicate constant {name}.{field}"));
                        }
                        let export = exported(target, name, &field);
                        methods
                            .extend(quote!(#export pub extern "C" fn #field() -> i32 { #value }));
                        signatures.extend(quote!(fn #field() -> i32;));
                    }
                    output.extend(quote!(pub struct #name; impl #name { #methods }));
                    exports.extend(quote!(class #name { #signatures }));
                }
                Item::Struct(s) => {
                    let class = &s.ident;
                    if !s.generics.params.is_empty() {
                        return Err("records cannot be generic".into());
                    }
                    let syn::Fields::Named(fields) = &s.fields else {
                        return Err("records need named fields".into());
                    };
                    let imported = idl_name(&s.attrs)?;
                    let source = imported.clone();
                    let mut overridden = HashSet::new();
                    let mut explicit_fields = Vec::new();
                    let mut extension_fields = Vec::new();
                    for field in &fields.named {
                        let named = (field.ident.clone().expect("named field"), field.ty.clone());
                        if extension(&field.attrs)? {
                            extension_fields.push(named);
                        } else {
                            explicit_fields.push(named);
                        }
                    }
                    let mut declared_fields: Vec<(syn::Ident, Type)> = if let Some(source) =
                        imported
                    {
                        let overrides: HashMap<_, _> = explicit_fields
                            .iter()
                            .map(|(name, ty)| (name.to_string(), ty.clone()))
                            .collect();
                        overridden = overrides.keys().cloned().collect();
                        let imported =
                            dictionary_fields(&idl, &source, &aliases, &idl_types, &overrides)?;
                        for name in overrides.keys() {
                            if !imported.iter().any(|(field, _)| field == name.as_str()) {
                                return Err(format!(
                                    "{class}.{name} does not override a member of {source}; mark a new member #[extension]"
                                ));
                            }
                        }
                        imported
                    } else {
                        explicit_fields
                    };
                    let extended: HashSet<String> = extension_fields
                        .iter()
                        .map(|(name, _)| name.to_string())
                        .collect();
                    // Members the backend has beyond the WebIDL dictionary.
                    declared_fields.extend(extension_fields);
                    described.push(convert::Record {
                        class: class.clone(),
                        source,
                        fields: declared_fields
                            .iter()
                            .map(|(name, ty)| {
                                let key = name.to_string();
                                let origin = if extended.contains(&key) {
                                    convert::Origin::Extension
                                } else if overridden.contains(&key) {
                                    convert::Origin::Override
                                } else {
                                    convert::Origin::Imported
                                };
                                (name.clone(), ty.clone(), origin)
                            })
                            .collect(),
                    });
                    let mut stored_fields = TokenStream::new();
                    let mut required_params = Vec::new();
                    let mut required_types = Vec::new();
                    let mut required_values = Vec::new();
                    let mut initial_values = Vec::new();
                    let mut methods = TokenStream::new();
                    let mut signatures = TokenStream::new();
                    let mut field_names = HashSet::new();
                    let mut method_names = HashSet::from(["new".to_owned()]);
                    for (field_name, field_ty) in declared_fields {
                        if !field_names.insert(field_name.to_string()) {
                            return Err(format!("duplicate field {class}.{field_name}"));
                        }
                        if let Some((key_ty, value_ty)) = generic_pair(&field_ty, "Map") {
                            for ty in [&key_ty, &value_ty] {
                                if type_name(ty).is_some_and(|name| unions.contains_key(&name)) {
                                    return Err(format!("{class}.{field_name} maps a union"));
                                }
                                if let Some(enumeration) = generic(ty, "Enum")
                                    && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                                {
                                    return Err(format!("unknown enum in {class}.{field_name}"));
                                }
                            }
                            let (stored_key, parameter_key, convert_key) =
                                stored_value(&key_ty, &resources, &records, target)?;
                            let (stored_value_ty, parameter_value, convert_value) =
                                stored_value(&value_ty, &resources, &records, target)?;
                            stored_fields.extend(
                            quote!(pub(crate) #field_name: Vec<(#stored_key, #stored_value_ty)>,),
                        );
                            initial_values.push(quote!(#field_name: Vec::new()));
                            let add =
                                ident(&format!("add{}", pascal(&field_name.unraw().to_string())))?;
                            if !method_names.insert(add.to_string()) {
                                return Err(format!(
                                    "generated method {class}.{add} is duplicated"
                                ));
                            }
                            let export = exported(target, class, &add);
                            methods.extend(quote! {
                                #export
                                pub extern "C" fn #add(
                                    this: &mut #class,
                                    key: #parameter_key,
                                    value: #parameter_value,
                                ) {
                                    let key = { let value = key; #convert_key };
                                    let value = { #convert_value };
                                    this.#field_name.push((key, value));
                                }
                            });
                            signatures.extend(
                                quote!(fn #add(&mut #class, #parameter_key, #parameter_value);),
                            );
                            continue;
                        }
                        let (container, value_ty) =
                            if let Some(inner) = generic(&field_ty, "Option") {
                                ("option", inner)
                            } else if let Some(inner) = generic(&field_ty, "Vec") {
                                ("sequence", inner)
                            } else {
                                ("required", field_ty)
                            };
                        let lowered_ty = if container == "sequence" {
                            generic(&value_ty, "Option").unwrap_or_else(|| value_ty.clone())
                        } else {
                            value_ty.clone()
                        };
                        let member = field_name.unraw().to_string();
                        if let Some(alternatives) =
                            type_name(&lowered_ty).and_then(|name| unions.get(&name))
                        {
                            // One setter per alternative. A required union is
                            // not a constructor argument; the backend checks it
                            // was set.
                            let union = &lowered_ty;
                            let sequence = container == "sequence";
                            if sequence && generic(&value_ty, "Option").is_some() {
                                return Err(format!("{class}.{field_name} holds nullable unions"));
                            }
                            if sequence {
                                stored_fields.extend(quote!(pub(crate) #field_name: Vec<#union>,));
                                initial_values.push(quote!(#field_name: Vec::new()));
                            } else {
                                stored_fields
                                    .extend(quote!(pub(crate) #field_name: Option<#union>,));
                                initial_values.push(quote!(#field_name: None));
                            }
                            for (variant, ty) in alternatives {
                                let (_, parameter, convert) =
                                    stored_value(ty, &resources, &records, target)?;
                                let setter = if sequence {
                                    ident(&format!("add{}{variant}", pascal(&member)))?
                                } else {
                                    ident(&format!("{member}{variant}"))?
                                };
                                if !method_names.insert(setter.to_string()) {
                                    return Err(format!(
                                        "generated method {class}.{setter} is duplicated"
                                    ));
                                }
                                let store = if sequence {
                                    quote!(this.#field_name.push(#union::#variant(#convert)))
                                } else {
                                    quote!(this.#field_name = Some(#union::#variant(#convert)))
                                };
                                let export = exported(target, class, &setter);
                                methods.extend(quote! {
                                #export
                                pub extern "C" fn #setter(this: &mut #class, value: #parameter) {
                                    #store;
                                }
                            });
                                signatures.extend(quote!(fn #setter(&mut #class, #parameter);));
                            }
                            continue;
                        }
                        if let Some(enumeration) = generic(&lowered_ty, "Enum")
                            && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                        {
                            return Err(format!("unknown enum in {class}.{field_name}"));
                        }
                        let (stored, parameter, convert) =
                            stored_value(&lowered_ty, &resources, &records, target)?;
                        match container {
                            "required" => {
                                stored_fields.extend(quote!(pub(crate) #field_name: #stored,));
                                required_params.push(quote!(#field_name: #parameter));
                                required_types.push(quote!(#parameter));
                                if convert.to_string() == "value" {
                                    required_values.push(quote!(#field_name));
                                } else {
                                    required_values.push(
                                        quote!(#field_name: { let value = #field_name; #convert }),
                                    );
                                }
                            }
                            "option" => {
                                if !method_names.insert(member.clone()) {
                                    return Err(format!(
                                        "generated method {class}.{field_name} is duplicated"
                                    ));
                                }
                                stored_fields
                                    .extend(quote!(pub(crate) #field_name: Option<#stored>,));
                                initial_values.push(quote!(#field_name: None));
                                let export = exported(target, class, &field_name);
                                methods.extend(quote! {
                                #export
                                pub extern "C" fn #field_name(this: &mut #class, value: #parameter) {
                                    this.#field_name = Some(#convert);
                                }
                            });
                                signatures.extend(quote!(fn #field_name(&mut #class, #parameter);));
                            }
                            "sequence" => {
                                let nullable = generic(&value_ty, "Option");
                                if nullable.is_some() {
                                    stored_fields.extend(
                                        quote!(pub(crate) #field_name: Vec<Option<#stored>>,),
                                    );
                                } else {
                                    stored_fields
                                        .extend(quote!(pub(crate) #field_name: Vec<#stored>,));
                                }
                                initial_values.push(quote!(#field_name: Vec::new()));
                                let add = ident(&format!("add{}", pascal(&member)))?;
                                if !method_names.insert(add.to_string()) {
                                    return Err(format!(
                                        "generated method {class}.{add} is duplicated"
                                    ));
                                }
                                if nullable.is_some() {
                                    let add_null = ident(&format!("{add}Null"))?;
                                    if !method_names.insert(add_null.to_string()) {
                                        return Err(format!(
                                            "generated method {class}.{add_null} is duplicated"
                                        ));
                                    }
                                    let export_add = exported(target, class, &add);
                                    let export_null = exported(target, class, &add_null);
                                    methods.extend(quote! {
                                    #export_add
                                    pub extern "C" fn #add(this: &mut #class, value: #parameter) {
                                        this.#field_name.push(Some(#convert));
                                    }
                                    #export_null
                                    pub extern "C" fn #add_null(this: &mut #class) {
                                        this.#field_name.push(None);
                                    }
                                });
                                    signatures.extend(quote! {
                                        fn #add(&mut #class, #parameter);
                                        fn #add_null(&mut #class);
                                    });
                                    continue;
                                }
                                let export = exported(target, class, &add);
                                methods.extend(quote! {
                                    #export
                                    pub extern "C" fn #add(this: &mut #class, value: #parameter) {
                                        this.#field_name.push(#convert);
                                    }
                                });
                                signatures.extend(quote!(fn #add(&mut #class, #parameter);));
                            }
                            _ => unreachable!(),
                        }
                    }
                    let constructor = ident("new")?;
                    let export = exported(target, class, &constructor);
                    methods.extend(quote! {
                        #export
                        pub extern "C" fn new(#(#required_params),*) -> Box<#class> {
                            Box::new(#class { #(#required_values,)* #(#initial_values,)* })
                        }
                    });
                    signatures = quote!(fn new(#(#required_types),*) -> Box<#class>; #signatures);
                    output.extend(quote! {
                        #[derive(Clone)]
                        pub struct #class { #stored_fields }
                        impl #class { #methods }
                    });
                    exports.extend(quote!(class #class { #signatures }));
                }
                Item::Trait(t) => {
                    let class = &t.ident;
                    if !t.generics.params.is_empty() || !t.supertraits.is_empty() {
                        return Err("resource traits cannot be generic or inherit".into());
                    }
                    let mut methods = TokenStream::new();
                    let mut signatures = TokenStream::new();
                    let mut statics = TokenStream::new();
                    // The variants types whose last value this class keeps.
                    let mut kept = HashSet::new();
                    let mut names = HashSet::new();
                    for method in &t.items {
                        let TraitItem::Fn(f) = method else {
                            return Err("resources contain only methods".into());
                        };
                        let name = &f.sig.ident;
                        if !names.insert(name.to_string()) {
                            return Err(format!("duplicate method {class}.{name}"));
                        }
                        if f.default.is_some()
                            || f.sig.asyncness.is_some()
                            || f.sig.unsafety.is_some()
                            || !f.sig.generics.params.is_empty()
                            || f.sig.variadic.is_some()
                        {
                            return Err(format!("unsupported signature {class}.{name}"));
                        }
                        let native = f
                            .attrs
                            .iter()
                            .find(|a| a.path().is_ident("native"))
                            .ok_or_else(|| format!("{class}.{name} needs #[native(function)]"))?
                            .parse_args::<syn::Ident>()
                            .map_err(error)?;
                        let member = idl_name(&f.attrs)?;
                        if let Some(source) = &member {
                            let declared = match &f.sig.output {
                                ReturnType::Default => syn::parse_quote!(()),
                                ReturnType::Type(_, ty) => (**ty).clone(),
                            };
                            check_member(&idl, source, &aliases, &idl_types, &declared, &untagged)
                                .map_err(|e| format!("{class}.{name}: {e}"))?;
                        }
                        let mut params = Vec::new();
                        let mut types = Vec::new();
                        let mut args = Vec::new();
                        let mut backend_types = Vec::new();
                        let mut declared = Vec::new();
                        // Whether an argument is an optional enum, which a
                        // runtime other than Caribou takes as its native
                        // value, `i32::MIN` for none, through `<name>Native`.
                        let mut lowered = false;
                        for (i, arg) in f.sig.inputs.iter().enumerate() {
                            let FnArg::Typed(arg) = arg else {
                                return Err("use an explicit this: &Class receiver".into());
                            };
                            let syn::Pat::Ident(pat) = &*arg.pat else {
                                return Err("arguments need simple names".into());
                            };
                            let param = &pat.ident;
                            let ty = &arg.ty;
                            let mut abi = quote!(#ty);
                            let value = if let Type::Reference(r) = &**ty {
                                let target =
                                    type_name(&r.elem).ok_or("invalid object reference")?;
                                if !classes.contains(&target) {
                                    return Err(format!("unknown resource {target}"));
                                }
                                // A record first is an argument to a static
                                // function; a resource first is the receiver.
                                if i == 0 && *class != target && resources.contains(&target) {
                                    return Err(format!(
                                        "first object parameter must be the {class} receiver"
                                    ));
                                }
                                if resources.contains(&target) {
                                    backend_types.push(quote!(i32));
                                    quote!(#param.handle)
                                } else {
                                    backend_types.push(quote!(#ty));
                                    quote!(#param)
                                }
                            } else if let Some(e) = generic(ty, "Enum") {
                                if !enums.contains(&type_name(&e).unwrap_or_default()) {
                                    return Err("unknown enum".into());
                                }
                                backend_types.push(quote!(i32));
                                quote!(#param.get().native())
                            } else if let Some(e) =
                                generic(ty, "Option").and_then(|o| generic(&o, "Enum"))
                            {
                                if !enums.contains(&type_name(&e).unwrap_or_default()) {
                                    return Err("unknown enum".into());
                                }
                                backend_types.push(quote!(Option<i32>));
                                if target == RustTarget::Caribou {
                                    quote!(#param.map(|v| v.get().native()))
                                } else {
                                    lowered = true;
                                    abi = quote!(i32);
                                    quote!((#param != i32::MIN).then_some(#param))
                                }
                            } else if scalar(ty) {
                                backend_types.push(quote!(#ty));
                                quote!(#param)
                            } else {
                                return Err(format!("unsupported argument type in {class}.{name}"));
                            };
                            params.push(quote!(#param: #abi));
                            types.push(abi);
                            args.push(value);
                            declared.push((**ty).clone());
                        }
                        let name = &if lowered {
                            let native = ident(&format!("{}Native", name.unraw()))?;
                            if !names.insert(native.to_string()) {
                                return Err(format!(
                                    "generated method {class}.{native} is duplicated"
                                ));
                            }
                            native
                        } else {
                            name.clone()
                        };
                        // A panic in the backend becomes a runtime error in the
                        // caller's language, and the fallback is returned.
                        let call = quote! {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                                backend::#native(#(#args),*)
                            }))
                        };
                        let raise_call = if target == RustTarget::Caribou {
                            quote!(caribou_abi::host::raise(
                                caribou_abi::ErrorKind::Runtime,
                                message
                            ))
                        } else {
                            quote!(host::raise(ErrorKind::Runtime, message))
                        };
                        let raise = quote! {
                            let message = error.downcast_ref::<String>().map(String::as_str)
                                .or_else(|| error.downcast_ref::<&str>().copied())
                                .unwrap_or("native backend panicked");
                            #raise_call;
                        };
                        if let ReturnType::Type(_, ty) = &f.sig.output
                            && type_name(ty).is_some_and(|n| variants.contains_key(&n))
                        {
                            if lowered {
                                return Err(format!(
                                    "{class}.{name} returns variants, so it cannot take an optional enum"
                                ));
                            }
                            if !backend_fns.iter().any(|b| b.name == native) {
                                backend_fns.push(BackendFn {
                                    name: native.clone(),
                                    params: backend_types.clone(),
                                    ret: quote!(-> #ty),
                                    fallback: quote!(<#ty>::default()),
                                    member: None,
                                });
                            }
                            let value = quote! {
                                match #call {
                                    Ok(value) => value,
                                    Err(error) => { #raise <#ty>::default() }
                                }
                            };
                            if target == RustTarget::Caribou {
                                methods.extend(quote! {
                                    pub extern "C" fn #name(#(#params),*) -> Enum<#ty> {
                                        let value: #ty = #value;
                                        value.into()
                                    }
                                });
                                signatures.extend(quote!(fn #name(#(#types),*) -> Enum<#ty>;));
                                continue;
                            }
                            // A runtime that cannot take the value whole takes
                            // its variant's index, then each field it holds
                            // through a getter that reads what the call kept.
                            // A class keeps one value of each variants type it
                            // returns, which every method returning it shares.
                            let kind = type_name(ty).expect("named variants");
                            let slot = quote::format_ident!("__XIDL_{}_{}", class, kind);
                            let index = ident(&format!("{}Variant", name.unraw()))?;
                            if !names.insert(index.to_string()) {
                                return Err(format!(
                                    "generated method {class}.{index} is duplicated"
                                ));
                            }
                            methods.extend(quote! {
                                pub extern "C" fn #index(#(#params),*) -> i32 {
                                    let value: #ty = #value;
                                    let index = value.variant();
                                    #slot.with(|slot| *slot.borrow_mut() = value);
                                    index
                                }
                            });
                            if kept.insert(kind.clone()) {
                                statics.extend(quote! {
                                    thread_local! {
                                        #[allow(non_upper_case_globals)]
                                        static #slot: std::cell::RefCell<#ty> =
                                            std::cell::RefCell::new(<#ty>::default());
                                    }
                                });
                                variant_getters(
                                    class,
                                    &slot,
                                    &variant_prefix(&kind),
                                    &kind,
                                    &[],
                                    &variants,
                                    &mut names,
                                    &mut methods,
                                )?;
                            }
                            continue;
                        }
                        let (return_type, convert, fallback) = match &f.sig.output {
                            ReturnType::Default => (quote!(), quote!(value), quote!(())),
                            ReturnType::Type(_, ty) => {
                                let (convert, fallback) = if let Some(target) = generic(ty, "Box") {
                                    if !resources.contains(&type_name(&target).unwrap_or_default())
                                    {
                                        return Err("unknown returned resource".into());
                                    }
                                    (quote!(Box::new(#target::from_handle(value))), quote!(0))
                                } else if let Some(enumeration) = generic(ty, "Enum") {
                                    if !enums.contains(&type_name(&enumeration).unwrap_or_default())
                                    {
                                        return Err("unknown returned enum".into());
                                    }
                                    {
                                        let raise = if target == RustTarget::Caribou {
                                            quote!(caribou_abi::host::raise(
                                                caribou_abi::ErrorKind::Type,
                                                "native enum value is not declared"
                                            ))
                                        } else {
                                            quote!(host::raise(
                                                ErrorKind::Type,
                                                "native enum value is not declared"
                                            ))
                                        };
                                        (
                                            quote! { match #enumeration::from_native(value) {
                                                Some(value) => value.into(),
                                                None => { #raise; #enumeration::default().into() }
                                            } },
                                            quote!(#enumeration::default().native()),
                                        )
                                    }
                                } else if scalar(ty) {
                                    let fallback = match type_name(ty).as_deref() {
                                        Some("Text") => quote!(Text::NULL),
                                        Some("Buffer") => quote!(Buffer::NULL),
                                        Some("Future") => quote!(Future::NULL),
                                        _ => quote!(Default::default()),
                                    };
                                    let fallback = if generic(ty, "Future").is_some() {
                                        quote!(Future::NULL)
                                    } else {
                                        fallback
                                    };
                                    (quote!(value), fallback)
                                } else {
                                    return Err("unsupported return type".into());
                                };
                                (quote!(-> #ty), convert, fallback)
                            }
                        };
                        if !backend_fns.iter().any(|b| b.name == native) {
                            let ret = match &f.sig.output {
                                ReturnType::Default => quote!(),
                                ReturnType::Type(_, ty)
                                    if generic(ty, "Box").is_some()
                                        || generic(ty, "Enum").is_some() =>
                                {
                                    quote!(-> i32)
                                }
                                ReturnType::Type(_, ty) => quote!(-> #ty),
                            };
                            backend_fns.push(BackendFn {
                                name: native.clone(),
                                params: backend_types.clone(),
                                ret,
                                fallback: fallback.clone(),
                                member: member.as_ref().map(|source| Member {
                                    source: source.clone(),
                                    class: class.clone(),
                                    args: declared.clone(),
                                    returns: match &f.sig.output {
                                        ReturnType::Default => None,
                                        ReturnType::Type(_, ty) => Some((**ty).clone()),
                                    },
                                }),
                            });
                        }
                        let body = if return_type.is_empty() {
                            quote!(if let Err(error) = #call { #raise })
                        } else if convert.to_string() == "value" {
                            quote! {
                                match #call {
                                    Ok(value) => value,
                                    Err(error) => { #raise #fallback }
                                }
                            }
                        } else {
                            quote! {
                                let value = match #call {
                                    Ok(value) => value,
                                    Err(error) => { #raise #fallback }
                                };
                                #convert
                            }
                        };
                        let export = exported(target, class, name);
                        methods.extend(quote! {
                            #export
                            pub extern "C" fn #name(#(#params),*) #return_type { #body }
                        });
                        signatures.extend(quote!(fn #name(#(#types),*) #return_type;));
                    }
                    let adapter_owned = target == RustTarget::Rayzor
                        && adapter_resources.contains(&class.to_string());
                    let (declaration, constructor) = if adapter_owned {
                        (quote!(), quote!())
                    } else {
                        (
                            quote! {
                                #[repr(C)]
                                #[derive(Default)]
                                pub struct #class { pub(crate) handle: i32 }
                            },
                            quote! {
                                #[allow(dead_code)]
                                pub(crate) fn from_handle(handle: i32) -> Self { Self { handle } }
                            },
                        )
                    };
                    output.extend(quote! {
                        #declaration
                        #statics
                        impl #class {
                            #constructor
                            #methods
                        }
                    });
                    exports.extend(quote!(class #class { #signatures }));
                }
                _ => unreachable!(),
            }
        }
    }
    let carries_bytes = variants.values().flatten().any(|(_, fields)| {
        fields
            .iter()
            .any(|(_, ty)| type_name(ty).as_deref() == Some("Buffer"))
    });
    if carries_bytes {
        output.extend(quote! {
            /// Bytes a variant holds, Rust's until the value reaches the
            /// language.
            #[derive(Debug, Clone, PartialEq, Default)]
            pub struct VariantBytes(pub Vec<u8>);
        });
        if target == RustTarget::Caribou {
            output.extend(quote! {
                impl caribou_abi::EnumField for VariantBytes {
                    const TAG: caribou_abi::TypeTag = caribou_abi::TypeTag::BUFFER;
                    fn into_value(self) -> caribou_abi::Value {
                        caribou_abi::Buffer::new(&self.0).value()
                    }
                    fn from_value(value: caribou_abi::Value) -> Self {
                        Self(caribou_abi::Buffer::of(value).map(|b| b.to_vec()).unwrap_or_default())
                    }
                }
            });
        }
    }
    let plugin = convert::Plugin {
        records: described,
        unions: unions
            .into_iter()
            .map(|(name, alternatives)| {
                let alternatives = alternatives
                    .into_iter()
                    .map(|(variant, ty)| {
                        let extra = union_extensions.contains(&(name.clone(), variant.to_string()));
                        (variant, ty, extra)
                    })
                    .collect();
                (name, alternatives)
            })
            .collect(),
        idl_types,
        resources,
        variants,
    };

    let output = if target == RustTarget::Caribou {
        quote!(#output caribou_abi::plugin! { name: #namespace; #exports })
    } else {
        output
    };

    Ok((output.to_string(), backend_fns, plugin))
}

/// A backend for a target that has only some of the backend's functions:
/// each function `implemented` (the source of a module, `crate::web`)
/// defines is forwarded to it. One it does not define whose method is
/// tagged with a WebIDL member, `#[idl("GPUTexture.width")]`, is generated
/// from that member over the module's `live`, `command`, `make`, `ask`,
/// `promise` and `release` (see `forward`). Every other one raises that it
/// is not available and returns what a failed call returns.
pub fn web_backend(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
    implemented: &str,
) -> Result<String, String> {
    web_backend_for(
        namespace,
        declaration.into(),
        webidl,
        implemented,
        RustTarget::Caribou,
    )
}

/// Generate the partial browser backend for a HashLink adapter. This uses
/// HashLink's resource wrappers and ABI carriers while sharing the same
/// WebGPU implementation and unavailable-operation fallbacks as Caribou.
pub fn hashlink_web_backend(
    namespace: &str,
    declaration: impl Into<Declaration>,
    webidl: &str,
    implemented: &str,
) -> Result<String, String> {
    web_backend_for(
        namespace,
        declaration.into(),
        webidl,
        implemented,
        RustTarget::HashLink,
    )
}

fn web_backend_for(
    namespace: &str,
    declaration: Declaration,
    webidl: &str,
    implemented: &str,
    target: RustTarget,
) -> Result<String, String> {
    let (_, backend_fns, plugin) =
        generate_parts(namespace, &declaration, webidl, target, &HashSet::new())?;
    let file = syn::parse_file(implemented).map_err(error)?;
    let defined: HashSet<String> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) if matches!(f.vis, syn::Visibility::Public(_)) => {
                Some(f.sig.ident.to_string())
            }
            _ => None,
        })
        .collect();
    let model = idl::parse(webidl)?;
    let (_, emitted) = wire::generate(&model);
    let mut out = convert::conversions(&plugin, &model, &emitted, &defined)?;
    let values = convert::Values::new(&plugin, &model, &emitted);
    for b in &backend_fns {
        let BackendFn {
            name,
            params,
            ret,
            fallback,
            member,
        } = b;
        let args: Vec<syn::Ident> = (0..params.len())
            .map(|i| quote::format_ident!("a{i}"))
            .collect();
        if defined.contains(&name.to_string()) {
            out.extend(quote! {
                pub unsafe fn #name(#(#args: #params),*) #ret { unsafe { crate::web::#name(#(#args),*) } }
            });
        } else if let Some(member) = member {
            out.extend(forward::generate(
                b,
                member,
                &model,
                &values,
                &plugin.resources,
            )?);
        } else {
            let message = format!("{namespace}: `{name}` is not available on the web");
            out.extend(quote! {
                pub unsafe fn #name(#(_: #params),*) #ret {
                    crate::runtime::host::raise(crate::runtime::ErrorKind::Runtime, #message);
                    #fallback
                }
            });
        }
    }
    Ok(out.to_string())
}

#[cfg(test)]
mod test {

    use crate::{
        enum_values, generate, generate_hashlink, generate_rayzor, pascal, tokens, web_backend,
    };

    #[test]
    fn an_optional_enum_argument_crosses_as_its_value_or_none() {
        let api = r#"
            enum Theme { Light, Dark }
            trait Window {
                #[native(window_set_theme)] fn setTheme(this: &Window, theme: Option<Enum<Theme>>);
            }
        "#;
        let library = crate::Library("window");
        // HashLink and Rayzor take the native value, i32::MIN for none,
        // through `setThemeNative`.
        for model in [
            library.generate_hashlink("window", api, "").unwrap(),
            library.generate_rayzor("window", api, "", &[]).unwrap(),
        ] {
            let flat = model.replace(' ', "");
            assert!(
                flat.contains("pubextern\"C\"fnsetThemeNative(this:&Window,theme:i32)"),
                "{model}"
            );
            assert!(
                flat.contains(
                    "backend::window_set_theme(this.handle,(theme!=i32::MIN).then_some(theme))"
                ),
                "{model}"
            );
        }
        // Caribou takes the option itself.
        let caribou = crate::generate_caribou("window", api, "")
            .unwrap()
            .replace(' ', "");
        assert!(
            caribou.contains("fnsetTheme(this:&Window,theme:Option<Enum<Theme>>)"),
            "{caribou}"
        );
        assert!(
            caribou.contains("theme.map(|v|v.get().native())"),
            "{caribou}"
        );
        for runtime in [crate::haxe::Runtime::HashLink, crate::haxe::Runtime::Rayzor] {
            let files = library.haxe("window", api, "", runtime).unwrap();
            let window = &files
                .iter()
                .find(|f| f.path == "window/Window.hx")
                .unwrap()
                .source;
            assert!(
                window.contains("inline function setTheme(theme:Null<Theme>):Void"),
                "{window}"
            );
            assert!(window.contains("setThemeNative("), "{window}");
            assert!(
                window.contains("(theme == null ? 0x80000000 : (theme : Int))"),
                "{window}"
            );
            // HashLink's symbol is `window_set_theme_native` in library
            // `window`; Rayzor's carries the library, `window_window_...`.
            assert!(window.contains("window_set_theme_native\")"), "{window}");
        }
    }

    #[test]
    fn rayzor_methods_name_classes_as_the_externs_do() {
        let api = r#"
            trait Device {
                #[native(device_name)] fn name(this: &Device) -> Text;
            }
        "#;
        let generated = crate::generate_rayzor_in("gpu", "rayzor.gpu", api, "", &[]).unwrap();
        let externs =
            crate::haxe::generate("rayzor.gpu", api, "", crate::haxe::Runtime::Rayzor).unwrap();
        assert!(generated.contains("\"rayzor::gpu::Device\""), "{generated}");
        let device = externs
            .iter()
            .find(|f| f.path.ends_with("Device.hx"))
            .expect("a Device extern");
        assert!(
            device.source.contains("@:native(\"rayzor::gpu::Device\")"),
            "{}",
            device.source
        );
    }

    #[test]
    fn rayzor_model_uses_adapter_carriers_and_exports_native_symbols() {
        let api = r#"
            enum Mode { Fast, Slow }
            struct Options { label: Option<Text>, mode: Enum<Mode> }
            trait Device {
                #[native(device_open)] fn open(options: &Options) -> Box<Device>;
                #[native(device_name)] fn name(this: &Device) -> Text;
            }
        "#;
        let generated = generate_rayzor("gpu", api, "").unwrap();

        assert!(!generated.contains("caribou_abi"));
        assert!(!generated.contains("plugin !"));
        assert!(generated.contains("Rooted < Text >"));
        assert!(generated.contains("impl NativeEnum for Mode"));
        assert!(generated.contains("export_name = \"xidl_options_new\""));
        assert!(generated.contains("export_name = \"xidl_device_open\""));
        assert!(generated.contains("host :: raise (ErrorKind :: Runtime"));
        assert!(generated.contains("pub static XIDL_METHODS"));
        assert!(generated.contains("\"gpu::Device\""));
        assert!(generated.contains("__xidl_device_open as * const u8"));
        assert!(generated.contains("fn __xidl_options_new (a0 : i64)"));
        assert!(generated.contains("transmute :: < i64 , Enum < Mode > >"));
        assert!(generated.contains("param_types : [3u8 , 0u8"));
    }

    #[test]
    fn webidl_comments_and_spacing_do_not_change_enum_values() {
        let idl = tokens(
            r#"// enum E { "wrong" };
          enum /* { } */ E { "one-minus-src", // comment with "quotes"
            "two", };
          enum Elsewhere { "ignored" };
        "#,
        )
        .unwrap();
        assert_eq!(enum_values(&idl, "E").unwrap(), ["one-minus-src", "two"]);
        assert_eq!(pascal("one-minus-src"), "OneMinusSrc");
        assert!(enum_values(&idl, "Missing").is_err());
        assert!(tokens("/* unterminated").is_err());
    }

    fn make_declaration(content: &str) -> crate::Declaration {
        content.into()
    }

    #[test]
    fn readonly_interface_attributes_can_generate_a_catalog_enum() {
        let idl = r#"
          typedef (GPUSampler or GPUBuffer or GPUBufferBinding or GPUExternalTexture) GPUResource;
          dictionary GPUBufferBinding { required GPUBuffer buffer; unsigned long long size; };
          interface GPUSupportedLimits { readonly attribute unsigned long maxTextureDimension1D; readonly attribute unsigned long long maxBufferSize; };
        "#;
        let generated = generate(
            "gpu",
            make_declaration(r#"#[idl("GPUSupportedLimits")] enum Limit {}"#),
            idl,
        )
        .unwrap();
        assert!(generated.contains("MaxTextureDimension1D"));
        assert!(generated.contains("MaxBufferSize"));
    }
    #[test]
    fn generated_code_contains_typed_objects_and_no_foreign_string_abi() {
        let generated = generate("gpu", make_declaration(r#"
          #[idl("Power")] enum Power {}
          #[idl("Usage")] mod Usage {}
          trait Device {
            #[native(create)] fn new() -> Box<Device>;
            #[native(shader)] fn shader(this: &Device, source: Text, data: Buffer, power: Enum<Power>) -> Box<Shader>;
          }
          trait Shader { #[native(name)] fn name(this: &Shader) -> Text; }
          trait Work { #[native(done)] fn done(this: &Work) -> Future; }
        "#), r#"enum Power { "low-power", "high-performance" }; namespace Usage { const Flags COPY = 0x4; };"#).unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("gpu.Power"));
        assert!(generated.contains(
            "backend :: shader (this . handle , source , data , power . get () . native ())"
        ));
        assert!(generated.contains("Box :: new (Shader :: from_handle (value))"));
        assert!(
            generated.contains(
                "fn shader (& Device , Text , Buffer , Enum < Power >) -> Box < Shader >"
            )
        );
        assert!(!generated.contains("wgpu.Power"));
        assert!(generated.contains("fn done (& Work) -> Future"));
    }
    #[test]
    fn promise_operations_map_to_the_shared_future_carrier() {
        let generated = generate(
            "gpu",
            make_declaration(
                r#"
              trait Queue {
                #[native(done)]
                #[idl("GPUQueue.onSubmittedWorkDone")]
                fn done(this: &Queue) -> Future<()>;
              }
            "#,
            ),
            "interface GPUQueue { Promise<undefined> onSubmittedWorkDone(); };",
        )
        .unwrap();
        assert!(generated.contains("fn done (& Queue) -> Future < () >"));

        let wrong = generate(
            "gpu",
            make_declaration(
                r#"
              trait Queue {
                #[native(done)]
                #[idl("GPUQueue.onSubmittedWorkDone")]
                fn done(this: &Queue) -> i32;
              }
            "#,
            ),
            "interface GPUQueue { Promise<undefined> onSubmittedWorkDone(); };",
        );
        assert!(wrong.is_err());
    }
    #[test]
    fn records_generate_required_optional_and_sequence_fields() {
        let generated = generate(
            "gpu",
            make_declaration(
                r#"
              enum Format { Rgba }
              struct Entry { slot: i32 }
              struct Descriptor {
                size: i64,
                label: Option<Text>,
                format: Option<Enum<Format>>,
                buffer: Option<BufferResource>,
                entries: Vec<Entry>,
              }
              trait BufferResource {}
              trait Device {
                #[native(create)] fn create(this: &Device, descriptor: &Descriptor);
              }
            "#,
            ),
            "",
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("pub (crate) size : i64"));
        assert!(
            generated.contains("pub (crate) label : Option < caribou_abi :: Rooted < Text > >")
        );
        assert!(generated.contains("pub (crate) format : Option < i32 >"));
        assert!(generated.contains("pub (crate) buffer : Option < i32 >"));
        assert!(generated.contains("pub (crate) entries : Vec < Entry >"));
        assert!(generated.contains("fn new (size : i64) -> Box < Descriptor >"));
        assert!(generated.contains("fn label (& mut Descriptor , Text)"));
        assert!(generated.contains("fn addEntries (& mut Descriptor , & Entry)"));
        assert!(generated.contains("backend :: create (this . handle , descriptor)"));
    }
    #[test]
    fn idl_records_import_inheritance_typedefs_defaults_and_sequences() {
        let generated = generate(
            "gpu",
            make_declaration(
                r#"
              #[idl("GPUFormat")] enum Format {}
              #[idl("GPUExtent")] struct Extent {}
              #[idl("GPUDescriptor")] struct Descriptor { extent: Extent }
            "#,
            ),
            r#"
              enum GPUFormat { "rgba", "depth" };
              dictionary GPUBase { DOMString label = ""; };
              typedef [EnforceRange] unsigned long long GPUSize;
              dictionary GPUExtent { required unsigned long width; };
              typedef (sequence<unsigned long> or GPUExtent) GPUExtentUnion;
              dictionary GPUDescriptor : GPUBase {
                required GPUSize size;
                required GPUExtentUnion extent;
                boolean enabled = false;
                sequence<GPUFormat> formats = [];
                record<DOMString, (GPUSize or undefined)> limits = {};
                sequence<GPUExtent?> layouts = [];
              };
            "#,
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(
            generated.contains("pub (crate) label : Option < caribou_abi :: Rooted < Text > >")
        );
        assert!(generated.contains("pub (crate) size : i64"));
        assert!(generated.contains("pub (crate) extent : Extent"));
        assert!(generated.contains("pub (crate) enabled : Option < bool >"));
        assert!(generated.contains("pub (crate) formats : Vec < i32 >"));
        assert!(
            generated
                .contains("pub (crate) limits : Vec < (caribou_abi :: Rooted < Text > , i64) >")
        );
        assert!(generated.contains("pub (crate) layouts : Vec < Option < Extent >>"));
        assert!(generated.contains("fn new (i64 , & Extent) -> Box < Descriptor >"));
        assert!(generated.contains("fn addFormats (& mut Descriptor , Enum < Format >)"));
        assert!(generated.contains("fn addLimits (& mut Descriptor , Text , i64)"));
        assert!(generated.contains("fn addLayouts (& mut Descriptor , & Extent)"));
        assert!(generated.contains("fn addLayoutsNull (& mut Descriptor)"));
    }
    #[test]
    fn keyword_members_keep_their_webidl_names() {
        let generated = generate(
            "gpu",
            make_declaration(
                r#"
              #[idl("GPUBindingType")] enum BindingType {}
              #[idl("GPULayout")] struct Layout {}
            "#,
            ),
            r#"
              enum GPUBindingType { "uniform", "storage" };
              dictionary GPULayout { GPUBindingType type = "uniform"; sequence<long> match = []; };
            "#,
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("pub (crate) r#type : Option < i32 >"));
        assert!(generated.contains("fn r#type (& mut Layout , Enum < BindingType >)"));
        assert!(generated.contains("fn addMatch (& mut Layout , i32)"));
    }
    #[test]
    fn declared_unions_take_one_setter_per_alternative() {
        let idl = r#"
          typedef (GPUSampler or GPUBuffer or GPUBufferBinding or GPUExternalTexture) GPUResource;
          dictionary GPUBufferBinding { required GPUBuffer buffer; unsigned long long size; };
          dictionary GPUEntry { required unsigned long binding; required GPUResource resource; };
          dictionary GPUGroup { sequence<GPUResource> extras = []; };
        "#;
        let generated = generate(
            "gpu",
            make_declaration(
                r#"
              #[idl("GPUSampler")] trait Sampler {}
              #[idl("GPUBuffer")] trait GpuBuffer {}
              #[idl("GPUBufferBinding")] struct BufferBinding {}
              #[idl("GPUResource")]
              enum Resource { Sampler(Sampler), Buffer(GpuBuffer), Binding(BufferBinding) }
              #[idl("GPUEntry")] struct Entry {}
              #[idl("GPUGroup")] struct Group {}
            "#,
            ),
            idl,
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains(
            "pub enum Resource { Sampler (i32) , Buffer (i32) , Binding (BufferBinding) }"
        ));
        assert!(generated.contains("pub (crate) resource : Option < Resource >"));
        assert!(generated.contains("fn new (i32) -> Box < Entry >"));
        assert!(generated.contains("fn resourceSampler (& mut Entry , & Sampler)"));
        assert!(generated.contains("fn resourceBinding (& mut Entry , & BufferBinding)"));
        assert!(generated.contains("this . resource = Some (Resource :: Buffer (value . handle))"));
        assert!(generated.contains("fn addExtrasBuffer (& mut Group , & GpuBuffer)"));
        assert!(
            !generated.contains("class Resource"),
            "a union is not a Caribou class"
        );

        let not_an_alternative = generate(
            "gpu",
            make_declaration(
                r#"
              trait Queue {}
              #[idl("GPUSampler")] trait Sampler {}
              #[idl("GPUResource")] enum Resource { Sampler(Sampler), Queue(Queue) }
            "#,
            ),
            idl,
        );
        assert!(not_an_alternative.is_err());
    }

    #[test]
    fn extensions_add_members_the_webidl_lacks() {
        let generated = generate(
            "gpu",
            make_declaration(
                r#"
              #[idl("GPUMode")] enum Mode { #[extension] Border }
              trait Array {}
              #[idl("GPUSampler")] trait Sampler {}
              #[idl("GPUResource")]
              enum Resource { Sampler(Sampler), #[extension] Array(Array) }
              #[idl("GPUDescriptor")] struct Descriptor {
                  /// Not in WebIDL.
                  #[extension] count: Option<i32>,
              }
              mod Statistic { const VERTEX: i32 = 1; const FRAGMENT: i32 = 4; }
              #[idl("GPUStage")] mod Stage { const EXTRA: i32 = 8; }
            "#,
            ),
            r#"
              enum GPUMode { "clamp", "repeat" };
              typedef (GPUSampler or GPUBuffer) GPUResource;
              dictionary GPUDescriptor { required GPUResource resource; };
              namespace GPUStage { const GPUFlags VERTEX = 0x1; };
            "#,
        )
        .unwrap();

        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("pub enum Mode { # [default] Clamp , Repeat , Border }"));
        assert!(generated.contains("Self :: Border => 2"));
        assert!(generated.contains("fn resourceArray (& mut Descriptor , & Array)"));
        assert!(generated.contains("fn count (& mut Descriptor , i32)"));
        assert!(generated.contains("fn FRAGMENT () -> i32"));
        assert!(generated.contains("fn VERTEX () -> i32"));
        assert!(generated.contains("fn EXTRA () -> i32"));
        assert!(
            generate("gpu", make_declaration("enum E { A, #[extension] B }"), "").is_err(),
            "an extension needs WebIDL values to extend"
        );
    }
    #[test]
    fn a_static_function_can_take_a_record_first() {
        let api = r#"
          struct Options { level: Option<i32> }
          trait Device {
              #[native(create_with)] fn createWith(options: &Options) -> Box<Device>;
          }
          trait Other {}
        "#;
        let generated = generate("gpu", make_declaration(api), "").unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("fn createWith (& Options) -> Box < Device >"));
        let foreign = api.replace("options: &Options", "other: &Other");
        assert!(
            generate("gpu", make_declaration(&foreign), "").is_err(),
            "a resource first must be the receiver"
        );
    }
    #[test]
    fn ambiguous_and_unsupported_declarations_fail_generation() {
        for (api, idl) in [
            ("#[idl(\"E\")] enum E {}", "enum E { \"a-b\", \"a--b\" };"),
            ("enum E {}", ""),
            ("trait R { fn call(this: &R); }", ""),
            ("trait R { #[native(call)] fn call(bytes: *mut u8); }", ""),
            (
                "trait R { #[native(call)] fn call(this: &R) -> Box<Missing>; }",
                "",
            ),
            (
                "#[idl(\"E\")] enum E {}",
                "enum E { \"a\" }; enum E { \"b\" };",
            ),
            ("struct R { new: Option<i32> }", ""),
        ] {
            assert!(
                generate("gpu", make_declaration(api), idl).is_err(),
                "accepted {api}"
            );
        }
    }

    const VARIANTS_API: &str = r#"
        enum Button { Left, Right }
        enum Event {
            None,
            Closed,
            Resized { width: i32, height: i32 },
            Pressed { button: Enum<Button>, text: Text, repeat: bool, at: f64 },
        }
        trait Window {
            #[native(window_poll)] fn poll(this: &Window) -> Event;
        }
    "#;

    #[test]
    fn caribou_takes_variants_whole() {
        let generated = generate("window", make_declaration(VARIANTS_API), "").unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("caribou_abi :: PluginEnum"));
        assert!(generated.contains("# [caribou (name = \"window.Event\")]"));
        assert!(
            generated
                .contains("Pressed { button : Button , text : String , repeat : bool , at : f64 }")
        );
        assert!(generated.contains("fn poll (this : & Window) -> Enum < Event >"));
        assert!(generated.contains("fn poll (& Window) -> Enum < Event > ;"));
        assert!(generated.contains("enum Event ;"));
    }

    #[test]
    fn rayzor_and_hashlink_read_variants_field_by_field() {
        let rayzor = generate_rayzor("window", make_declaration(VARIANTS_API), "").unwrap();
        syn::parse_file(&rayzor).unwrap();
        assert!(rayzor.contains("static __XIDL_Window_Event"));
        assert!(rayzor.contains("fn pollVariant (this : & Window) -> i32"));
        assert!(rayzor.contains("fn eventResizedWidth () -> i32"));
        assert!(rayzor.contains("fn eventPressedText () -> Text"));
        assert!(rayzor.contains("fn eventPressedButton () -> Enum < Button >"));
        assert!(rayzor.contains("export_name = \"xidl_window_event_pressed_text\""));
        assert!(!rayzor.contains("PluginEnum"));

        let hashlink = generate_hashlink("window", make_declaration(VARIANTS_API), "").unwrap();
        syn::parse_file(&hashlink).unwrap();
        assert!(hashlink.contains("hlp_window_poll_variant"));
        assert!(hashlink.contains("\"Pi_i\""));
        assert!(hashlink.contains("hlp_window_event_pressed_at"));
        assert!(hashlink.contains("\"P_d\""));

        for runtime in [crate::haxe::Runtime::Rayzor, crate::haxe::Runtime::HashLink] {
            let files =
                crate::haxe::generate("window", make_declaration(VARIANTS_API), "", runtime)
                    .unwrap();
            let file = |name: &str| {
                files
                    .iter()
                    .find(|f| f.path == format!("window/{name}.hx"))
                    .unwrap()
                    .source
                    .clone()
            };
            assert!(file("Event").contains(
                "enum Event {\n\tNone;\n\tClosed;\n\tResized(width:Int, height:Int);\n\tPressed(button:Button, text:String, repeat:Bool, at:Float);\n}"
            ));
            let window = file("Window");
            assert!(
                window.contains("inline function poll():Event {"),
                "{window}"
            );
            assert!(window.contains("case 1: Event.Closed;"), "{window}");
            assert!(window.contains("default: Event.None;"), "{window}");
        }
    }

    #[test]
    fn hashlink_carries_f32_as_the_double_haxe_declares() {
        let api = r#"
            trait Sampler {
                #[native(sampler_scale)] fn scale(this: &Sampler, by: f32) -> f32;
            }
        "#;
        let hashlink = generate_hashlink("gpu", make_declaration(api), "").unwrap();
        syn::parse_file(&hashlink).unwrap();
        assert!(hashlink.contains("\"Pid_d\""), "{hashlink}");
        assert!(hashlink.contains("a1 : f64) -> f64"), "{hashlink}");
        assert!(hashlink.contains("a1 as f32"), "{hashlink}");
        assert!(hashlink.contains("value as f64"), "{hashlink}");

        let files = crate::haxe::generate(
            "gpu",
            make_declaration(api),
            "",
            crate::haxe::Runtime::HashLink,
        )
        .unwrap();
        let sampler = &files
            .iter()
            .find(|f| f.path == "gpu/Sampler.hx")
            .unwrap()
            .source;
        assert!(sampler.contains("by:Float"), "{sampler}");
    }

    #[test]
    fn variants_are_results_of_what_every_runtime_carries() {
        for api in [
            "enum E { A, B { x: Vec<i32> } }",
            "enum E { A, B { x: i32 } } trait R { #[native(f)] fn f(this: &R, e: E); }",
            "enum E { A, B { x: i32 } } struct S { e: E }",
            "enum E { A, B { x: Enum<Missing> } }",
            "enum E { A, B { x: Missing } }",
            "enum A { X { b: B } } enum B { Y { a: A } }",
        ] {
            assert!(
                generate("w", make_declaration(api), "").is_err(),
                "accepted {api}"
            );
        }
    }

    const NESTED_API: &str = r#"
        enum Code { Unknown, KeyA }
        enum Text2 { None, Some { text: Text } }
        enum Physical { Code { code: Enum<Code> }, Native { scancode: i64 } }
        enum KeyEvent { Input { physical: Physical, text: Text2, repeat: bool } }
        enum Path { Utf8 { path: Text }, Bytes { bytes: Buffer } }
        enum Event {
            None,
            Key { device: i32, event: KeyEvent },
            Dropped { path: Path },
        }
        trait Window {
            #[native(window_poll)] fn poll(this: &Window) -> Event;
        }
    "#;

    #[test]
    fn variants_nest_and_carry_bytes() {
        let caribou = generate("window", make_declaration(NESTED_API), "").unwrap();
        syn::parse_file(&caribou).unwrap();
        assert!(caribou.contains("Key { device : i32 , event : KeyEvent }"));
        assert!(caribou.contains("Bytes { bytes : VariantBytes }"));
        assert!(caribou.contains("impl caribou_abi :: EnumField for VariantBytes"));
        assert!(caribou.contains("enum KeyEvent ;"));
        // A first variant with fields defaults them.
        assert!(caribou.contains(
            "Self :: Input { physical : Default :: default () , text : Default :: default () , repeat : Default :: default () }"
        ));

        let rayzor = generate_rayzor("window", make_declaration(NESTED_API), "").unwrap();
        syn::parse_file(&rayzor).unwrap();
        assert!(!rayzor.contains("EnumField"));
        for getter in [
            "fn pollVariant (this : & Window) -> i32",
            "fn eventKeyEventVariant () -> i32",
            "fn eventKeyEventInputPhysicalVariant () -> i32",
            "fn eventKeyEventInputPhysicalCodeCode () -> Enum < Code >",
            "fn eventKeyEventInputPhysicalNativeScancode () -> i64",
            "fn eventKeyEventInputTextSomeText () -> Text",
            "fn eventDroppedPathBytesBytes () -> Buffer",
        ] {
            assert!(rayzor.contains(getter), "{getter}");
        }
        assert!(rayzor.contains("Buffer :: new (& found . 0)"));

        for runtime in [crate::haxe::Runtime::Rayzor, crate::haxe::Runtime::HashLink] {
            let files =
                crate::haxe::generate("window", make_declaration(NESTED_API), "", runtime).unwrap();
            let file = |name: &str| {
                files
                    .iter()
                    .find(|f| f.path == format!("window/{name}.hx"))
                    .unwrap()
                    .source
                    .clone()
            };
            assert!(
                file("KeyEvent").contains("Input(physical:Physical, text:Text2, repeat:Bool);")
            );
            assert!(file("Path").contains("Bytes(bytes:haxe.io.Bytes);"));
            let window = file("Window");
            assert!(
                window.contains("static inline function readEvent(index:Int):Event {"),
                "{window}"
            );
            assert!(
                window.contains("static inline function eventKeyEvent():KeyEvent {"),
                "{window}"
            );
            assert!(
                window.contains("static inline function eventKeyEventInputPhysical():Physical {")
            );
            assert!(window.contains("case 1: Event.Key("), "{window}");
        }
    }

    #[test]
    fn a_library_names_its_natives_and_record_abstracts() {
        let api = r#"
            struct Options { label: Option<Text> }
            trait Device {
                #[native(open)] fn open(options: &Options) -> Box<Device>;
                #[native(read)] fn read(this: &Device) -> Buffer;
                #[native(write)] fn write(this: &Device, data: Buffer);
            }
        "#;
        let library = crate::Library("xwindow");
        let hashlink = library
            .generate_hashlink("window", make_declaration(api), "")
            .unwrap();
        assert!(hashlink.contains("\"PXxwindow_Options__i\""));
        assert!(hashlink.contains("\"Pi_Xxwindow_buffer_result_\""));
        // A Buffer argument is a haxe.io.Bytes: its length, then its bytes.
        assert!(hashlink.contains("\"PiOiB__v\""));
        assert!(hashlink.contains("a1 : * mut runtime :: HlBytes"));
        assert!(hashlink.contains("\"PXxwindow_buffer_result_OiB__v\""));
        let rayzor = library
            .generate_rayzor("window", make_declaration(api), "", &[])
            .unwrap();
        assert!(rayzor.contains("export_name = \"xwindow_device_open\""));
        let files = library
            .haxe(
                "window",
                make_declaration(api),
                "",
                crate::haxe::Runtime::HashLink,
            )
            .unwrap();
        let all: String = files.iter().map(|f| f.source.as_str()).collect();
        assert!(all.contains("@:hlNative(\"xwindow\", \"device_read\")"));
        assert!(all.contains("hl.Abstract<\"xwindow_Options\">"));
        assert!(all.contains("hl.Abstract<\"xwindow_buffer_result\">"));
        assert!(!all.contains("\"xidl"));
        assert!(
            crate::Library("x-window")
                .generate_hashlink("w", None, "")
                .is_err()
        );
    }

    #[test]
    fn an_unparsable_declaration_is_refused() {
        assert!(generate("w", make_declaration("trait R {"), "").is_err());
    }

    #[test]
    fn a_tagged_method_the_web_module_lacks_is_generated_from_its_member() {
        let idl = r#"
          enum GPUTextureFormat { "r8unorm", "rgba8unorm" };
          interface GPUTexture {
            readonly attribute unsigned long width;
            readonly attribute GPUTextureFormat format;
            readonly attribute USVString label;
            undefined destroy();
          };
          interface GPUSampler {};
          interface GPURenderPipeline {};
          interface GPURenderBundleEncoder {};
          interface mixin GPURenderCommandsMixin {
            undefined draw(unsigned long vertexCount, optional unsigned long instanceCount = 1);
          };
          GPURenderBundleEncoder includes GPURenderCommandsMixin;
          dictionary GPUSamplerDescriptor { USVString label = ""; };
          dictionary GPURenderPipelineDescriptor { required USVString entry; };
          interface GPUDevice {
            GPUSampler createSampler(optional GPUSamplerDescriptor descriptor = {});
            Promise<GPURenderPipeline> createRenderPipelineAsync(GPURenderPipelineDescriptor descriptor);
            undefined pushErrorScope(unsigned long filter);
          };
        "#;
        let declaration = r#"
          #[idl("GPUTextureFormat")] enum Format { R8unorm, Rgba8unorm }
          #[idl("GPUSamplerDescriptor")] struct SamplerDescriptor {}
          #[idl("GPURenderPipelineDescriptor")] struct PipelineDescriptor {}
          #[idl("GPUSampler")] trait Sampler {}
          trait Pipeline {}
          #[idl("GPUTexture")]
          trait Texture {
              #[idl("GPUTexture.width")] #[native(texture_width)]
              fn width(this: &Texture) -> i32;
              #[idl("GPUTexture.format")] #[native(texture_format)]
              fn format(this: &Texture) -> Enum<Format>;
              #[idl("GPUTexture.label")] #[native(texture_label)]
              fn label(this: &Texture) -> Text;
              #[idl("GPUTexture.destroy")] #[native(texture_destroy)]
              fn destroy(this: &Texture);
          }
          #[idl("GPURenderBundleEncoder")]
          trait Bundle {
              #[idl("GPURenderBundleEncoder.draw")] #[native(bundle_draw)]
              fn draw(this: &Bundle, vertices: i32);
          }
          #[idl("GPUDevice")]
          trait Device {
              #[idl("GPUDevice.createSampler")] #[native(sampler_create)]
              fn sampler(this: &Device, descriptor: &SamplerDescriptor) -> Box<Sampler>;
              #[idl("GPUDevice.createRenderPipelineAsync")] #[native(pipeline_create_async)]
              fn pipeline(this: &Device, descriptor: &PipelineDescriptor) -> Future<Pipeline>;
              #[idl("GPUDevice.pushErrorScope")] #[native(error_scope_push)]
              fn pushErrorScope(this: &Device, filter: i32);
          }
        "#;
        // A hand-written function wins over its member.
        let web = "pub fn error_scope_push(device: i32, filter: i32) {}";
        let generated = web_backend("gpu", declaration, idl, web).unwrap();
        syn::parse_file(&generated).unwrap();
        let flat = generated.replace(' ', "");
        for expected in [
            "if!crate::web::live(\"GPUTexture\",a0){return",
            "crate::web::ask::<u32>(|e,reply|e.gpu_texture_get_width(crate::wire::Handle(a0asu32),reply))",
            "Some(v)=>vasi32",
            "crate::web::ask::<crate::wire::GPUTextureFormat>",
            "Some(v)=>vasu32asi32",
            "crate::web::ask::<String>",
            "Some(v)=>crate::runtime::Text::new(&v)",
            "crate::web::command(|e|e.gpu_texture_destroy(crate::wire::Handle(a0asu32)));crate::web::release(\"GPUTexture\",a0);",
            // A mixin's member, its optional argument left out.
            "if!crate::web::live(\"GPURenderBundleEncoder\",a0)",
            "Ok((*(&a1)asu32,None,))",
            "crate::web::make(\"GPUSampler\",|e,made|e.gpu_device_create_sampler(crate::wire::Handle(a0asu32),made,&w0))",
            "Ok((Some(a1.wire()?),))",
            // A resource importing no interface stands for the member's.
            "crate::web::promise::<crate::Pipeline>(Some(\"GPURenderPipeline\"),",
            "|future,handle|future.resolve_boxed(Box::new(crate::Pipeline{handle}))",
            "pubunsafefnerror_scope_push(a0:i32,a1:i32){unsafe{crate::web::error_scope_push(a0,a1)}}",
        ] {
            assert!(flat.contains(expected), "{expected} in {generated}");
        }

        // A tag the web cannot follow is an error naming the remedy.
        let bad = declaration.replace(
            "fn draw(this: &Bundle, vertices: i32);",
            "fn draw(this: &Bundle, vertices: i32, instances: i32, more: i32);",
        );
        let error = web_backend("gpu", bad, idl, web).unwrap_err();
        assert!(
            error.contains("Bundle.bundle_draw (GPURenderBundleEncoder.draw) cannot be generated"),
            "{error}"
        );
        assert!(
            error.contains("define `bundle_draw` in the web module, or untag it"),
            "{error}"
        );

        // A tag whose member returns something else is refused when declared.
        let wrong = declaration.replace(
            "fn width(this: &Texture) -> i32;",
            "fn width(this: &Texture) -> Text;",
        );
        let error = web_backend("gpu", wrong, idl, web).unwrap_err();
        assert!(
            error.contains("Texture.width: returns Text, but GPUTexture.width maps to i32"),
            "{error}"
        );
    }

    #[test]
    fn a_web_backend_converts_records_to_the_wires_dictionaries() {
        let idl = r#"
          enum GPUFilterMode { "nearest", "linear" };
          enum GPUAutoLayoutMode { "auto" };
          interface GPUBuffer {};
          interface GPUSampler {};
          interface GPUPipelineLayout {};
          interface GPUDevice {
            undefined make(GPUThing descriptor);
            undefined lay(GPULaid descriptor);
          };
          typedef (GPUSampler or GPUBuffer or GPUBinding) GPUResource;
          dictionary GPUBinding { required GPUBuffer buffer; GPUSize64 size; };
          typedef [EnforceRange] unsigned long long GPUSize64;
          dictionary GPUThing {
            USVString label = "";
            required GPUSize64 size;
            GPUFilterMode filter = "nearest";
            sequence<GPUResource> resources = [];
          };
          dictionary GPULaid { required (GPUPipelineLayout or GPUAutoLayoutMode) layout; };
          dictionary GPUExtent3DDict { required unsigned long width; };
          typedef (sequence<unsigned long> or GPUExtent3DDict) GPUExtent3D;
          dictionary GPUSized { required GPUExtent3D size; };
          interface GPUQueue { undefined size(GPUSized descriptor); };
        "#;
        let declaration = r#"
          #[idl("GPUFilterMode")] enum Filter { #[extension] Cubic }
          #[idl("GPUBuffer")] trait Buffer {}
          #[idl("GPUSampler")] trait Sampler {}
          #[idl("GPUPipelineLayout")] trait Layout {}
          #[idl("GPUDevice")] trait Device {}
          #[idl("GPUBinding")] struct Binding {}
          #[idl("GPUResource")]
          enum Resource { Sampler(Sampler), Buffer(Buffer), Binding(Binding) }
          #[idl("GPUThing")] struct Thing { #[extension] native: Option<i32> }
          #[idl("GPULaid")] struct Laid { layout: Option<Layout> }
          #[idl("GPUExtent3DDict")] struct Extent {}
          #[idl("GPUSized")] struct Sized { size: Extent }
        "#;
        let web = "pub fn laid_layout() {}";
        let generated = web_backend("gpu", declaration, idl, web).unwrap();
        syn::parse_file(&generated).unwrap();
        let flat = generated.replace(' ', "");
        for expected in [
            "implcrate::Thing{",
            "fnwire(&self)->Result<crate::wire::GPUThing,String>",
            "ifself.native.is_some(){returnErr(\"`Thing.native`isnotavailableontheweb\".to_owned());}",
            "label:match&self.label{Some(x)=>Some(x.get().as_str().to_owned()),None=>None}",
            "size:*(&self.size)asu64",
            "filter:match&self.filter{Some(x)=>Some(crate::wire::GPUFilterMode::from_index(*xasu32)",
            "crate::Resource::Buffer(x)=>crate::wire::GPUResource::GPUBuffer(crate::wire::Handle(*xasu32))",
            "crate::Resource::Binding(x)=>crate::wire::GPUResource::GPUBinding(x.wire()?)",
            "layout:crate::web::laid_layout(&self.layout)?",
            // An override pairs with the union alternative that is its
            // dictionary.
            "size:crate::wire::GPUExtent3D::GPUExtent3DDict((&self.size).wire()?)",
        ] {
            assert!(flat.contains(expected), "{expected} in {generated}");
        }
    }
}
