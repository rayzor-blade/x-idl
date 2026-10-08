use std::path::PathBuf;

fn main() -> Result<(), String> {
    let usage = "usage: xidl-typescript <namespace> <declaration.rs> <webidl.idl> <output.ts>";
    let mut args = std::env::args_os().skip(1);
    let namespace = args
        .next()
        .and_then(|v| v.into_string().ok())
        .ok_or(usage)?;
    let declaration = PathBuf::from(args.next().ok_or(usage)?);
    let webidl = std::fs::read_to_string(args.next().ok_or(usage)?).map_err(|e| e.to_string())?;
    let output = PathBuf::from(args.next().ok_or(usage)?);
    if args.next().is_some() {
        return Err(usage.into());
    }
    let binding = x_idl::typescript::generate(&namespace, declaration, &webidl)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(output, binding.source).map_err(|e| e.to_string())
}
