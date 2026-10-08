fn main() {
    napi_build::setup();
    let api = std::fs::read_to_string("api.rs").unwrap();
    let binding = x_idl::node::generate("test", api, "").unwrap();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("binding.rs"), binding.rust).unwrap();
    if std::fs::read_to_string("generated.ts").ok().as_deref() != Some(&binding.typescript) {
        std::fs::write("generated.ts", binding.typescript).unwrap();
    }
    println!("cargo:rerun-if-changed=api.rs");
}
