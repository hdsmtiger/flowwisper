use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FLOWWISPER_SQLCIPHER_STATIC");
    println!("cargo:rerun-if-env-changed=FLOWWISPER_SQLCIPHER_KEY");

    // Enable FTS5 and JSON1 when building the bundled SQLCipher library.
    println!("cargo:rustc-cfg=flowwisper_sqlcipher");
    println!("cargo:rustc-env=SQLCIPHER_ENABLE_FTS5=1");
    println!("cargo:rustc-env=SQLCIPHER_ENABLE_JSON1=1");

    if let Ok(target) = env::var("CARGO_CFG_TARGET_OS") {
        if target == "macos" {
            println!("cargo:rustc-link-arg=-Wl,-undefined,dynamic_lookup");
        }
        if target == "windows" {
            println!("cargo:rustc-link-lib=dylib=advapi32");
        }
    }
}
