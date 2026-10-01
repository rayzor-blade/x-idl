use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1).peekable();
    let usage = "usage: xidl-haxe --idl path/to/plugin.idl --namespace \"my.plugin\" <--declaration (optional)> <path/to/declaration.rs> <hashlink|ash|rayzor|javascript> <output-directory>";
    args.next();
    let idl_path = PathBuf::from(args.next().ok_or(usage)?);
    args.next();
    let namespace = args.next().and_then(|v| v.into_string().ok());
    // check if next argument is a declaration or a path to a declaration file
    let has_declaraion = args.peek().map(|v| v.to_string_lossy() == "--declaration").ok_or(usage)?;
    if has_declaraion {
         args.next();
    }
    let declaration_path = args.next().map(PathBuf::from);
   
    let target = args.next().and_then(|v| v.into_string().ok());

    let root = PathBuf::from(args.next().ok_or(usage)?);
    if args.next().is_some() {
        return Err(usage.into());
    }
    let files = match target.as_deref() {
        Some("hashlink") | Some("ash") => x_idl::haxe(&namespace.unwrap_or("xidl".to_string()), declaration_path, idl_path.to_str().ok_or(usage)?, x_idl::haxe::Runtime::HashLink)?,
        Some("rayzor") => x_idl::haxe(&namespace.unwrap_or("xidl".to_string()), declaration_path, idl_path.to_str().ok_or(usage)?, x_idl::haxe::Runtime::Rayzor)?,
        // Some("javascript") | Some("js") => x_idl::haxe_js::generate(x_idl::idl::parse(idl_path.to_str().ok_or(usage)?)?)?,
        _ => return Err(usage.into()),
    };
    for file in files {
        let path = root.join(file.path);
        std::fs::create_dir_all(path.parent().expect("generated file has a parent"))
            .map_err(|e| e.to_string())?;
        std::fs::write(path, file.source).map_err(|e| e.to_string())?;
    }
    Ok(())
}
