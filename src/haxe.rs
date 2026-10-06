//! Conventional Haxe externs generated from the same declaration as the
//! Caribou plugin. Runtime-specific annotations and future carriers are the
//! only differences between targets.

use std::collections::HashMap;

use quote::ToTokens;
use syn::ext::IdentExt;
use syn::{FnArg, GenericArgument, Item, PathArguments, ReturnType, TraitItem, Type};

/// The native binding convention an extern set targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// HashLink HDLL symbols loaded from the binding's library; Promise
    /// results use Ash's externally completable Future carrier.
    HashLink,
    /// Rayzor package methods and `rayzor.concurrent.Future<T>`.
    Rayzor,
}

/// One generated source file, relative to a Haxe class path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub source: String,
}

/// Emit the conventional Haxe extern surface for a runtime, its natives in
/// the `xidl` library.
pub fn generate(
    namespace: &str,
    declaration: impl Into<crate::Declaration>,
    webidl: &str,
    runtime: Runtime,
) -> Result<Vec<File>, String> {
    generate_in("xidl", namespace, declaration.into(), webidl, runtime)
}

/// As `generate`, its natives in `library` (see `crate::Library`).
pub(crate) fn generate_in(
    library: &str,
    namespace: &str,
    declaration: crate::Declaration,
    webidl: &str,
    runtime: Runtime,
) -> Result<Vec<File>, String> {
    let desc_content = declaration.text()?;

    let file = syn::parse_file(&desc_content);
    let docs = file.as_ref().map(collect_docs).unwrap_or_default();

    let schema_namespace = namespace.rsplit('.').next().unwrap_or(namespace);
    let (_, _, plugin) = super::generate_parts(
        schema_namespace,
        &declaration,
        webidl,
        super::RustTarget::Caribou,
        &std::collections::HashSet::new(),
    )?;
    let records: HashMap<_, _> = plugin
        .records
        .iter()
        .map(|r| (r.class.to_string(), r))
        .collect();
    let mut out = Vec::new();

    if runtime == Runtime::HashLink {
        out.push(source(
            namespace,
            "XidlBytes",
            r#"@:noCompletion
class XidlBytes {
	public static function take(value:hl.Abstract<"LIBRARY_buffer_result">):haxe.io.Bytes {
		if (value == null) return null;
		var out = haxe.io.Bytes.alloc(XidlBytesNative.length(value));
		XidlBytesNative.copy(value, out);
		return out;
	}
}

private extern class XidlBytesNative {
	@:hlNative("LIBRARY", "buffer_result_len")
	public static function length(value:hl.Abstract<"LIBRARY_buffer_result">):Int;
	@:hlNative("LIBRARY", "buffer_result_copy")
	public static function copy(value:hl.Abstract<"LIBRARY_buffer_result">, out:haxe.io.Bytes):Void;
}
"#
            .replace("LIBRARY", library),
        ));
    }
    if let Ok(file) = file {
        for item in file.items {
            match item {
                Item::Enum(item) => {
                    if plugin.unions.contains_key(&item.ident.to_string()) {
                        continue;
                    }
                    let name = item.ident.to_string();
                    if let Some(declared) = plugin.variants.get(&name) {
                        out.push(source(
                            namespace,
                            &name,
                            variants_enum(&name, declared, runtime, &docs)?,
                        ));
                        continue;
                    }
                    let mut variants = Vec::new();
                    let imported = super::idl_name(&item.attrs)?;
                    if let Some(source) = imported {
                        let model = super::idl::parse(webidl)?;
                        if let Some(values) = model.enums.get(&source) {
                            variants.extend(
                                values
                                    .iter()
                                    .enumerate()
                                    .map(|(i, v)| (super::pascal(v), Some(i as i32))),
                            );
                        } else if let Some(interface) = model.interface(&source) {
                            variants.extend(
                                interface
                                    .attributes
                                    .iter()
                                    .filter(|a| a.readonly)
                                    .enumerate()
                                    .map(|(i, a)| (super::pascal(&a.name), Some(i as i32))),
                            );
                        } else {
                            return Err(format!("WebIDL enum or catalog {source} was not found"));
                        }
                    }
                    for variant in item.variants {
                        let value = variant
                            .discriminant
                            .as_ref()
                            .and_then(|(_, e)| super::discriminant(e));
                        variants.push((variant.ident.to_string(), value));
                    }
                    let mut next = 0;
                    let body = variants
                        .into_iter()
                        .map(|(variant, explicit)| {
                            let value = explicit.unwrap_or(next);
                            next = value.saturating_add(1);
                            let doc = comment(&docs, &format!("{name}.{variant}"), &[], "\t");
                            format!("{doc}\tvar {variant} = {value};")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let doc = comment(&docs, &name, &[], "");
                    out.push(source(
                        namespace,
                        &name,
                        format!("{doc}enum abstract {name}(Int) from Int to Int {{\n{body}\n}}\n"),
                    ));
                }
                Item::Mod(item) => {
                    let Some((_, items)) = item.content else {
                        continue;
                    };
                    let name = item.ident.to_string();
                    let mut constants = Vec::new();
                    if let Some(imported) = super::idl_name(&item.attrs)? {
                        let tokens = super::tokens(webidl)?;
                        let body = super::body(&tokens, "namespace", &imported)?;
                        for statement in body.split(|token| token == ";").filter(|s| !s.is_empty())
                        {
                            if statement.len() != 5
                                || statement[0] != "const"
                                || statement[3] != "="
                            {
                                return Err(format!("unsupported constant in {imported}"));
                            }
                            constants.push(format!(
                                "\tpublic static inline var {}:Int = {};",
                                statement[2], statement[4]
                            ));
                        }
                    }
                    constants.extend(items.into_iter().filter_map(|item| match item {
                        Item::Const(c) => Some(format!(
                            "{}\tpublic static inline var {}:Int = {};",
                            comment(&docs, &format!("{name}.{}", c.ident), &[], "\t"),
                            c.ident,
                            c.expr.to_token_stream()
                        )),
                        _ => None,
                    }));
                    let constants = constants.join("\n");
                    let doc = comment(&docs, &name, &[], "");
                    out.push(source(
                        namespace,
                        &name,
                        format!("{doc}class {name} {{\n{constants}\n}}\n"),
                    ));
                }
                Item::Struct(item) => {
                    let name = item.ident.to_string();
                    let record = records
                        .get(&name)
                        .ok_or_else(|| format!("record {name} was not described"))?;
                    if runtime == Runtime::HashLink {
                        out.push(hashlink_record(
                            library, namespace, &name, record, &plugin, &docs,
                        )?);
                        continue;
                    }
                    let mut required = Vec::new();
                    let mut params = Vec::new();
                    let mut methods = Vec::new();
                    let doc = |field: &str| comment(&docs, &format!("{name}.{field}"), &[], "\t");
                    for (field, ty, _) in &record.fields {
                        let field = field.to_string().trim_start_matches("r#").to_owned();
                        if let Some((key, value)) = pair(ty, "Map") {
                            let method = format!("add{}", super::pascal(&field));
                            methods.push(
                                doc(&field)
                                    + &method_line(
                                        library,
                                        runtime,
                                        &name,
                                        &method,
                                        &format!(
                                            "key:{}, value:{}",
                                            hx_type(&key, runtime)?,
                                            hx_type(&value, runtime)?
                                        ),
                                        "Void",
                                    ),
                            );
                        } else {
                            let (container, mut value) = if let Some(value) = one(ty, "Option") {
                                ("option", value)
                            } else if let Some(value) = one(ty, "Vec") {
                                ("sequence", value)
                            } else {
                                ("required", ty.clone())
                            };
                            if container == "sequence" {
                                value = one(&value, "Option").unwrap_or(value);
                            }
                            if let Some(alternatives) =
                                plugin.unions.get(&simple_name(&value).unwrap_or_default())
                            {
                                for (variant, ty, _) in alternatives {
                                    let method = if container == "sequence" {
                                        format!("add{}{variant}", super::pascal(&field))
                                    } else {
                                        format!("{field}{variant}")
                                    };
                                    methods.push(
                                        doc(&field)
                                            + &method_line(
                                                library,
                                                runtime,
                                                &name,
                                                &method,
                                                &format!("value:{}", hx_type(ty, runtime)?),
                                                "Void",
                                            ),
                                    );
                                }
                            } else if container == "sequence" {
                                let method = format!("add{}", super::pascal(&field));
                                methods.push(
                                    doc(&field)
                                        + &method_line(
                                            library,
                                            runtime,
                                            &name,
                                            &method,
                                            &format!("value:{}", hx_type(&value, runtime)?),
                                            "Void",
                                        ),
                                );
                            } else if container == "option" {
                                methods.push(
                                    doc(&field)
                                        + &method_line(
                                            library,
                                            runtime,
                                            &name,
                                            &field,
                                            &format!("value:{}", hx_type(&value, runtime)?),
                                            "Void",
                                        ),
                                );
                            } else {
                                required.push(format!("{field}:{}", hx_type(&value, runtime)?));
                                params.push((field.clone(), format!("{name}.{field}")));
                            }
                        }
                    }
                    let constructor = native(library, runtime, &name, "new");
                    let mut body = format!(
                        "{}{}extern class {name} {{\n{}\t{constructor}\n\tpublic function new({});",
                        comment(&docs, &name, &[], ""),
                        class_annotation(runtime, namespace, &name),
                        comment(&docs, "", &params, "\t"),
                        required.join(", ")
                    );
                    if !methods.is_empty() {
                        body.push('\n');
                        body.push_str(&methods.join("\n"));
                    }
                    body.push_str("\n}\n");
                    out.push(source(namespace, &name, body));
                }
                Item::Trait(item) => {
                    let name = item.ident.to_string();
                    if runtime == Runtime::HashLink {
                        out.push(hashlink_resource(
                            library, namespace, &name, &item, &plugin, &docs,
                        )?);
                        continue;
                    }
                    let mut methods = Vec::new();
                    // The variants types whose readers this class has.
                    let mut kept = std::collections::HashSet::new();
                    for entry in item.items {
                        let TraitItem::Fn(method) = entry else {
                            continue;
                        };
                        let rust_name = method.sig.ident.to_string();
                        let doc = comment(&docs, &format!("{name}.{rust_name}"), &[], "\t");
                        let mut args = Vec::new();
                        let mut typed = Vec::new();
                        let mut instance = false;
                        for (at, arg) in method.sig.inputs.iter().enumerate() {
                            let FnArg::Typed(arg) = arg else { continue };
                            let syn::Pat::Ident(param) = &*arg.pat else {
                                continue;
                            };
                            if at == 0
                                && param.ident == "this"
                                && reference_name(&arg.ty).as_deref() == Some(name.as_str())
                            {
                                instance = true;
                                continue;
                            }
                            args.push(format!("{}:{}", param.ident, hx_type(&arg.ty, runtime)?));
                            typed.push((param.ident.to_string(), (*arg.ty).clone()));
                        }
                        let ret = match &method.sig.output {
                            ReturnType::Default => "Void".to_owned(),
                            ReturnType::Type(_, ty) => hx_type(ty, runtime)?,
                        };
                        let static_ = if instance { "" } else { "static " };
                        if let Some(variants_name) = returned_variants(&method.sig.output, &plugin)
                        {
                            // The value is built here from its variant's index
                            // and the fields the natives read back.
                            let index = format!("{rust_name}Variant");
                            methods.push(format!(
                                "\t{}\n\tprivate {static_}function {index}({}):Int;",
                                native(library, runtime, &name, &index),
                                args.join(", ")
                            ));
                            if kept.insert(variants_name.clone()) {
                                let call = |getter: &str| format!("{getter}()");
                                let leaf = |value: String, _: &Type| value;
                                let mut reader = Reader::new(&plugin, &call, &leaf);
                                reader.read(&variants_name);
                                for (getter, ty) in &reader.natives {
                                    let ty = match ty {
                                        Some(ty) => hx_type(ty, runtime)?,
                                        None => "Int".to_owned(),
                                    };
                                    methods.push(format!(
                                        "\t{}\n\tprivate static function {getter}():{ty};",
                                        native(library, runtime, &name, getter),
                                    ));
                                }
                                methods.extend(reader.helpers);
                            }
                            let names = arg_names(&args);
                            methods.push(format!(
                                "{doc}\tpublic {static_}inline function {rust_name}({}):{ret} {{\n\t\treturn read{variants_name}({index}({names}));\n\t}}",
                                args.join(", ")
                            ));
                            continue;
                        }
                        if let Some((native_args, call_args)) = lowered(&typed, runtime)? {
                            // An optional enum crosses as its value; the
                            // public method passes none as NONE.
                            let lowered_name = format!("{rust_name}Native");
                            methods.push(format!(
                                "\t{}\n\tprivate {static_}function {lowered_name}({native_args}):{ret};",
                                native(library, runtime, &name, &lowered_name),
                            ));
                            let call = format!("{lowered_name}({call_args})");
                            let body = if ret == "Void" {
                                call
                            } else {
                                format!("return {call}")
                            };
                            methods.push(format!(
                                "{doc}\tpublic {static_}inline function {rust_name}({}):{ret} {{\n\t\t{body};\n\t}}",
                                args.join(", ")
                            ));
                            continue;
                        }
                        let annotation = native(library, runtime, &name, &rust_name);
                        if rust_name == "new" {
                            methods.push(format!(
                                "{doc}\t{annotation}\n\tpublic function new({});",
                                args.join(", ")
                            ));
                        } else {
                            methods.push(format!(
                                "{doc}\t{annotation}\n\tpublic {static_}function {rust_name}({}):{ret};",
                                args.join(", ")
                            ));
                        }
                    }
                    let body = format!(
                        "{}{}extern class {name} {{\n{}\n}}\n",
                        comment(&docs, &name, &[], ""),
                        class_annotation(runtime, namespace, &name),
                        methods.join("\n")
                    );
                    out.push(source(namespace, &name, body));
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// A record's setter, its parameters, and the field whose doc it carries.
type Setter = (String, Vec<(String, Type)>, String);

fn hashlink_record(
    library: &str,
    namespace: &str,
    name: &str,
    record: &super::convert::Record,
    plugin: &super::convert::Plugin,
    docs: &Docs,
) -> Result<File, String> {
    let mut required = Vec::new();
    let mut methods: Vec<Setter> = Vec::new();
    for (field, ty, _) in &record.fields {
        let field = field.to_string().trim_start_matches("r#").to_owned();
        if let Some((key, value)) = pair(ty, "Map") {
            methods.push((
                format!("add{}", super::pascal(&field)),
                vec![("key".into(), key), ("value".into(), value)],
                field,
            ));
            continue;
        }
        let (container, mut value) = if let Some(value) = one(ty, "Option") {
            ("option", value)
        } else if let Some(value) = one(ty, "Vec") {
            ("sequence", value)
        } else {
            ("required", ty.clone())
        };
        if container == "sequence" {
            value = one(&value, "Option").unwrap_or(value);
        }
        if let Some(alternatives) = plugin.unions.get(&simple_name(&value).unwrap_or_default()) {
            for (variant, ty, _) in alternatives {
                let method = if container == "sequence" {
                    format!("add{}{variant}", super::pascal(&field))
                } else {
                    format!("{field}{variant}")
                };
                methods.push((method, vec![("value".into(), ty.clone())], field.clone()));
            }
        } else if container == "sequence" {
            methods.push((
                format!("add{}", super::pascal(&field)),
                vec![("value".into(), value)],
                field,
            ));
        } else if container == "option" {
            methods.push((field.clone(), vec![("value".into(), value)], field));
        } else {
            required.push((field, value));
        }
    }
    let abstract_ty = format!("hl.Abstract<\"{library}_{name}\">");
    let args = haxe_args(&required, Runtime::HashLink)?;
    let names = required
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let params: Vec<(String, String)> = required
        .iter()
        .map(|(field, _)| (field.clone(), format!("{name}.{field}")))
        .collect();
    let mut public = format!(
        "{}abstract {name}({abstract_ty}) {{\n{}\tpublic inline function new({args}) this = {name}Native.create({names});",
        comment(docs, name, &[], ""),
        comment(docs, "", &params, "\t"),
    );
    let mut native_class = format!(
        "private extern class {name}Native {{\n\t{}\n\tpublic static function create({args}):{abstract_ty};",
        native(library, Runtime::HashLink, name, "new"),
    );
    for (method, params, field) in methods {
        let args = haxe_args(&params, Runtime::HashLink)?;
        let names = params
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let comma = if names.is_empty() { "" } else { ", " };
        public.push_str(&format!(
            "\n{}\tpublic inline function {method}({args}):Void {name}Native.{method}(this{comma}{names});",
            comment(docs, &format!("{name}.{field}"), &[], "\t"),
        ));
        native_class.push_str(&format!(
            "\n\t{}\n\tpublic static function {method}(self:{abstract_ty}{comma}{args}):Void;",
            native(library, Runtime::HashLink, name, &method),
        ));
    }
    public.push_str("\n}\n\n");
    native_class.push_str("\n}\n");
    Ok(source(namespace, name, format!("{public}{native_class}")))
}

fn hashlink_resource(
    library: &str,
    namespace: &str,
    name: &str,
    item: &syn::ItemTrait,
    plugin: &super::convert::Plugin,
    docs: &Docs,
) -> Result<File, String> {
    let mut public = format!(
        "{}abstract {name}(Int) from Int to Int {{",
        comment(docs, name, &[], "")
    );
    let mut native_class = format!("private extern class {name}Native {{");
    let mut text_getters = false;
    // The variants types whose readers this class has.
    let mut kept = std::collections::HashSet::new();
    for entry in &item.items {
        let TraitItem::Fn(method) = entry else {
            continue;
        };
        let rust_name = method.sig.ident.to_string();
        let doc = comment(docs, &format!("{name}.{rust_name}"), &[], "\t");
        let mut params = Vec::new();
        let mut instance = false;
        for (at, arg) in method.sig.inputs.iter().enumerate() {
            let FnArg::Typed(arg) = arg else { continue };
            let syn::Pat::Ident(param) = &*arg.pat else {
                continue;
            };
            if at == 0 && param.ident == "this" && reference_name(&arg.ty).as_deref() == Some(name)
            {
                instance = true;
                continue;
            }
            params.push((param.ident.to_string(), (*arg.ty).clone()));
        }
        let args = haxe_args(&params, Runtime::HashLink)?;
        let names = params
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let ret_ty = match &method.sig.output {
            ReturnType::Default => syn::parse_quote!(()),
            ReturnType::Type(_, ty) => (**ty).clone(),
        };
        let ret = hx_type(&ret_ty, Runtime::HashLink)?;
        if let Some(variants_name) = returned_variants(&method.sig.output, plugin) {
            let index = format!("{rust_name}Variant");
            let native_args = if instance {
                if args.is_empty() {
                    "self:Int".to_owned()
                } else {
                    format!("self:Int, {args}")
                }
            } else {
                args.clone()
            };
            native_class.push_str(&format!(
                "\n\t{}\n\tpublic static function {index}({native_args}):Int;",
                native(library, Runtime::HashLink, name, &index),
            ));
            let call_args = match (instance, names.is_empty()) {
                (true, true) => "this".to_owned(),
                (true, false) => format!("this, {names}"),
                (false, _) => names.clone(),
            };
            if kept.insert(variants_name.clone()) {
                let call = |getter: &str| format!("{name}Native.{getter}()");
                let leaf = |value: String, ty: &Type| match simple_name(ty).as_deref() {
                    Some("Text") => format!("text({value})"),
                    Some("Buffer") => format!("XidlBytes.take({value})"),
                    _ => value,
                };
                let mut reader = Reader::new(plugin, &call, &leaf);
                reader.read(&variants_name);
                for (getter, ty) in &reader.natives {
                    let native_ty = match ty.as_ref().map(|ty| (simple_name(ty), ty)) {
                        None => "Int".to_owned(),
                        Some((Some(n), _)) if n == "Text" => {
                            text_getters = true;
                            "hl.Bytes".to_owned()
                        }
                        Some((Some(n), _)) if n == "Buffer" => {
                            format!("hl.Abstract<\"{library}_buffer_result\">")
                        }
                        Some((_, ty)) => hx_type(ty, Runtime::HashLink)?,
                    };
                    native_class.push_str(&format!(
                        "\n\t{}\n\tpublic static function {getter}():{native_ty};",
                        native(library, Runtime::HashLink, name, getter),
                    ));
                }
                for helper in reader.helpers {
                    public.push('\n');
                    public.push_str(&helper);
                }
            }
            let static_ = if instance { "" } else { "static " };
            public.push_str(&format!(
                "\n{doc}\tpublic {static_}inline function {rust_name}({args}):{ret} {{\n\t\treturn read{variants_name}({name}Native.{index}({call_args}));\n\t}}"
            ));
            continue;
        }
        let native_ret = match simple_name(&ret_ty).as_deref() {
            Some("Text") => "hl.Bytes".to_owned(),
            Some("Buffer") => format!("hl.Abstract<\"{library}_buffer_result\">"),
            _ => ret.clone(),
        };
        let native_args = if instance {
            if args.is_empty() {
                "self:Int".to_owned()
            } else {
                format!("self:Int, {args}")
            }
        } else {
            args.clone()
        };
        // An optional enum crosses as its value; the public method passes
        // none as NONE.
        let lowered = lowered(&params, Runtime::HashLink)?;
        let (native_args, names, native_name) = match &lowered {
            Some((args, names)) => (
                if instance {
                    if args.is_empty() {
                        "self:Int".to_owned()
                    } else {
                        format!("self:Int, {args}")
                    }
                } else {
                    args.clone()
                },
                names.clone(),
                format!("{rust_name}Native"),
            ),
            None => (native_args, names, rust_name.clone()),
        };
        let native_method = if rust_name == "new" {
            "create"
        } else {
            &native_name
        };
        native_class.push_str(&format!(
            "\n\t{}\n\tpublic static function {native_method}({native_args}):{native_ret};",
            native(library, Runtime::HashLink, name, &native_name),
        ));
        let call_args = if instance {
            if names.is_empty() {
                "this".to_owned()
            } else {
                format!("this, {names}")
            }
        } else {
            names
        };
        let call = format!("{name}Native.{native_method}({call_args})");
        let body = match simple_name(&ret_ty).as_deref() {
            Some("Text") => format!(
                "{{ var value = {call}; return value == null ? null : @:privateAccess String.fromUCS2(value); }}"
            ),
            Some("Buffer") => format!("return XidlBytes.take({call})"),
            _ if matches!(&ret_ty, Type::Tuple(tuple) if tuple.elems.is_empty()) => {
                call.to_string()
            }
            _ => format!("return {call}"),
        };
        if rust_name == "new" {
            public.push_str(&format!(
                "\n{doc}\tpublic inline function new({args}) this = {call};"
            ));
        } else {
            let static_ = if instance { "" } else { "static " };
            public.push_str(&format!(
                "\n{doc}\tpublic {static_}inline function {rust_name}({args}):{ret} {body};"
            ));
        }
    }
    if text_getters {
        public.push_str(
            "\n\tstatic inline function text(value:hl.Bytes):String {\n\t\treturn value == null ? null : @:privateAccess String.fromUCS2(value);\n\t}",
        );
    }
    public.push_str("\n}\n\n");
    native_class.push_str("\n}\n");
    Ok(source(namespace, name, format!("{public}{native_class}")))
}

/// The name of the declared variants a method returns, if it returns some.
fn returned_variants(output: &ReturnType, plugin: &super::convert::Plugin) -> Option<String> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    simple_name(ty).filter(|name| plugin.variants.contains_key(name))
}

/// A Haxe enum of the declared variants, each with its fields.
fn variants_enum(
    name: &str,
    declared: &super::Variants,
    runtime: Runtime,
    docs: &Docs,
) -> Result<String, String> {
    let mut body = Vec::new();
    for (variant, fields) in declared {
        let params: Vec<(String, String)> = fields
            .iter()
            .map(|(field, _)| {
                let field = field.unraw().to_string();
                let key = format!("{name}.{variant}.{field}");
                (field, key)
            })
            .collect();
        let doc = comment(docs, &format!("{name}.{variant}"), &params, "\t");
        if fields.is_empty() {
            body.push(format!("{doc}\t{variant};"));
            continue;
        }
        let fields = fields
            .iter()
            .map(|(field, ty)| Ok(format!("{}:{}", field.unraw(), hx_type(ty, runtime)?)))
            .collect::<Result<Vec<_>, String>>()?;
        body.push(format!("{doc}\t{variant}({});", fields.join(", ")));
    }
    let doc = comment(docs, name, &[], "");
    Ok(format!("{doc}enum {name} {{\n{}\n}}\n", body.join("\n")))
}

/// How a Haxe surface reads a variants value: from its variant's index,
/// then each field through a native getter, a nested value through a
/// helper that reads it the same way.
struct Reader<'a> {
    plugin: &'a super::convert::Plugin,
    /// Each native getter called, and the type of the field it reads, or
    /// none for a nested value's index.
    natives: Vec<(String, Option<Type>)>,
    /// The helpers that build nested values.
    helpers: Vec<String>,
    /// A call of a native getter.
    call: &'a dyn Fn(&str) -> String,
    /// A field's value from the call reading it.
    leaf: &'a dyn Fn(String, &Type) -> String,
}

impl<'a> Reader<'a> {
    fn new(
        plugin: &'a super::convert::Plugin,
        call: &'a dyn Fn(&str) -> String,
        leaf: &'a dyn Fn(String, &Type) -> String,
    ) -> Self {
        Reader {
            plugin,
            natives: Vec::new(),
            helpers: Vec::new(),
            call,
            leaf,
        }
    }

    /// `read<Name>(index)`, which builds a `name` from its variant's index
    /// and the getters of what the class kept.
    fn read(&mut self, name: &str) {
        let body = self.build(name, &super::variant_prefix(name), "index");
        self.helpers.push(format!(
            "\tstatic inline function read{name}(index:Int):{name} {{\n\t\treturn {body};\n\t}}"
        ));
    }

    /// The expression building the `name` whose variant `index` gives, its
    /// fields read by getters beneath `prefix`.
    fn build(&mut self, name: &str, prefix: &str, index: &str) -> String {
        let declared = &self.plugin.variants[name];
        let mut values = Vec::new();
        for (variant, fields) in declared {
            let mut args = Vec::new();
            for (field, ty) in fields {
                let getter = super::variant_getter(prefix, variant, field);
                let nested = simple_name(ty).filter(|n| self.plugin.variants.contains_key(n));
                if let Some(nested) = nested {
                    let index = format!("{getter}Variant");
                    self.natives.push((index.clone(), None));
                    let body = self.build(&nested, &getter, &(self.call)(&index));
                    self.helpers.push(format!(
                        "\tstatic inline function {getter}():{nested} {{\n\t\treturn {body};\n\t}}"
                    ));
                    args.push(format!("{getter}()"));
                } else {
                    self.natives.push((getter.clone(), Some(ty.clone())));
                    args.push((self.leaf)((self.call)(&getter), ty));
                }
            }
            values.push(if args.is_empty() {
                format!("{name}.{variant}")
            } else {
                format!("{name}.{variant}({})", args.join(", "))
            });
        }
        if values.len() == 1 {
            return values.remove(0);
        }
        let mut arms = String::new();
        for (at, value) in values.iter().enumerate().skip(1) {
            arms.push_str(&format!("\t\t\tcase {at}: {value};\n"));
        }
        format!(
            "switch ({index}) {{\n{arms}\t\t\tdefault: {};\n\t\t}}",
            values[0]
        )
    }
}

/// The declaration's `///` text, by item (`Window`) and by member
/// (`Window.setTheme`, `Theme.Dark`, `WindowAttributes.title`).
type Docs = HashMap<String, Vec<String>>;

fn doc_lines(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .map(|line| {
            line.strip_prefix(' ')
                .unwrap_or(&line)
                .trim_end()
                .to_owned()
        })
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn collect_docs(file: &syn::File) -> Docs {
    let mut docs = Docs::new();
    let mut put = |key: String, attrs: &[syn::Attribute]| {
        let lines = doc_lines(attrs);
        if !lines.is_empty() {
            docs.insert(key, lines);
        }
    };
    for item in &file.items {
        match item {
            Item::Enum(e) => {
                put(e.ident.to_string(), &e.attrs);
                for v in &e.variants {
                    put(format!("{}.{}", e.ident, v.ident), &v.attrs);
                    for f in &v.fields {
                        if let Some(field) = &f.ident {
                            put(
                                format!("{}.{}.{}", e.ident, v.ident, field.unraw()),
                                &f.attrs,
                            );
                        }
                    }
                }
            }
            Item::Struct(s) => {
                put(s.ident.to_string(), &s.attrs);
                for f in &s.fields {
                    if let Some(field) = &f.ident {
                        put(format!("{}.{}", s.ident, field.unraw()), &f.attrs);
                    }
                }
            }
            Item::Trait(t) => {
                put(t.ident.to_string(), &t.attrs);
                for entry in &t.items {
                    if let TraitItem::Fn(f) = entry {
                        put(format!("{}.{}", t.ident, f.sig.ident), &f.attrs);
                    }
                }
            }
            Item::Mod(m) => {
                put(m.ident.to_string(), &m.attrs);
                for c in m.content.iter().flat_map(|(_, items)| items) {
                    if let Item::Const(c) = c {
                        put(format!("{}.{}", m.ident, c.ident), &c.attrs);
                    }
                }
            }
            _ => {}
        }
    }
    docs
}

/// `key`'s doc as a Haxe doc comment indented by `indent`, then a line of
/// `@param` for each of `params` that has one; nothing when there is none.
fn comment(docs: &Docs, key: &str, params: &[(String, String)], indent: &str) -> String {
    let mut lines = docs.get(key).cloned().unwrap_or_default();
    for (name, param_key) in params {
        if let Some(text) = docs.get(param_key) {
            lines.push(format!("@param {name} {}", text.join(" ")));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = lines.iter().map(|l| l.replace("*/", "*\\/")).collect();
    if let [line] = lines.as_slice() {
        return format!("{indent}/** {line} */\n");
    }
    let mut out = format!("{indent}/**\n");
    for line in &lines {
        if line.is_empty() {
            out.push_str(&format!("{indent} *\n"));
        } else {
            out.push_str(&format!("{indent} * {line}\n"));
        }
    }
    out.push_str(&format!("{indent} */\n"));
    out
}

/// What an optional enum argument stands for when it is none: `i32::MIN`,
/// which no declared enum value is.
const NONE: &str = "0x80000000";

/// For a method taking an optional enum: its native's parameters, the enum
/// as its value, and the call's arguments, none as `NONE`.
fn lowered(args: &[(String, Type)], runtime: Runtime) -> Result<Option<(String, String)>, String> {
    let optional = |ty: &Type| one(ty, "Option").is_some_and(|inner| one(&inner, "Enum").is_some());
    if !args.iter().any(|(_, ty)| optional(ty)) {
        return Ok(None);
    }
    let mut native = Vec::new();
    let mut call = Vec::new();
    for (name, ty) in args {
        if optional(ty) {
            native.push(format!("{name}:Int"));
            call.push(format!("({name} == null ? {NONE} : ({name} : Int))"));
        } else {
            native.push(format!("{name}:{}", hx_type(ty, runtime)?));
            call.push(name.clone());
        }
    }
    Ok(Some((native.join(", "), call.join(", "))))
}

/// The names in a Haxe parameter list, `a:Int, b:Float` as `a, b`.
fn arg_names(args: &[String]) -> String {
    args.iter()
        .map(|arg| arg.split(':').next().unwrap_or(arg))
        .collect::<Vec<_>>()
        .join(", ")
}

fn haxe_args(args: &[(String, Type)], runtime: Runtime) -> Result<String, String> {
    args.iter()
        .map(|(name, ty)| Ok(format!("{name}:{}", hx_type(ty, runtime)?)))
        .collect::<Result<Vec<_>, String>>()
        .map(|args| args.join(", "))
}

fn source(namespace: &str, name: &str, body: String) -> File {
    File {
        path: format!("{}/{name}.hx", namespace.replace('.', "/")),
        source: format!(
            "// Generated by xidl-bindgen. Do not edit by hand.\npackage {namespace};\n\n{body}"
        ),
    }
}

fn method_line(
    library: &str,
    runtime: Runtime,
    class: &str,
    method: &str,
    args: &str,
    ret: &str,
) -> String {
    format!(
        "\t{}\n\tpublic function {method}({args}):{ret};",
        native(library, runtime, class, method)
    )
}

fn class_annotation(runtime: Runtime, namespace: &str, class: &str) -> String {
    match runtime {
        Runtime::HashLink => String::new(),
        Runtime::Rayzor => format!("@:native(\"{}::{class}\")\n", namespace.replace('.', "::")),
    }
}

fn native(library: &str, runtime: Runtime, class: &str, method: &str) -> String {
    let symbol = format!("{}_{}", snake(class), snake(method));
    match runtime {
        Runtime::HashLink => format!("@:hlNative(\"{library}\", \"{symbol}\")"),
        Runtime::Rayzor => format!("@:native(\"{library}_{symbol}\")"),
    }
}

pub(crate) fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn hx_type(ty: &Type, runtime: Runtime) -> Result<String, String> {
    if let Type::Reference(reference) = ty {
        return hx_type(&reference.elem, runtime);
    }
    if let Type::Tuple(tuple) = ty
        && tuple.elems.is_empty()
    {
        return Ok("Void".into());
    }
    if let Some(inner) = one(ty, "Option") {
        return Ok(format!("Null<{}>", hx_type(&inner, runtime)?));
    }
    if let Some(inner) = one(ty, "Vec") {
        return Ok(format!("Array<{}>", hx_type(&inner, runtime)?));
    }
    if let Some(inner) = one(ty, "Enum").or_else(|| one(ty, "Box")) {
        return hx_type(&inner, runtime);
    }
    if let Some(inner) = one(ty, "Future") {
        let inner = hx_type(&inner, runtime)?;
        return Ok(match runtime {
            Runtime::HashLink => format!("ash.Future<{inner}>"),
            Runtime::Rayzor => format!("rayzor.concurrent.Future<{inner}>"),
        });
    }
    if let Some((key, value)) = pair(ty, "Map") {
        return Ok(format!(
            "Map<{}, {}>",
            hx_type(&key, runtime)?,
            hx_type(&value, runtime)?
        ));
    }
    let name = simple_name(ty)
        .ok_or_else(|| format!("unsupported Haxe type: {}", ty.to_token_stream()))?;
    Ok(match name.as_str() {
        "i32" | "u32" => "Int".into(),
        "i64" | "u64" => "haxe.Int64".into(),
        "f32" | "f64" => "Float".into(),
        "bool" => "Bool".into(),
        "Text" => "String".into(),
        "Buffer" | "BufferMut" => "haxe.io.Bytes".into(),
        other => other.to_owned(),
    })
}

fn one(ty: &Type, name: &str) -> Option<Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(ty) if args.args.len() == 1 => Some(ty.clone()),
        _ => None,
    }
}

fn pair(ty: &Type, name: &str) -> Option<(Type, Type)> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
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
    Some((types.next()?, types.next()?))
}

fn simple_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else { return None };
    path.path.segments.last().map(|s| s.ident.to_string())
}

fn reference_name(ty: &Type) -> Option<String> {
    let Type::Reference(reference) = ty else {
        return None;
    };
    simple_name(&reference.elem)
}
