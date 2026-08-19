use std::{env, path::PathBuf};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let plist = manifest_dir.join("../../packaging/macos/Info.plist");
    println!("cargo:rerun-if-changed={}", plist.display());
    if !plist.is_file() {
        println!(
            "cargo:warning=macOS Info.plist is missing at {}",
            plist.display()
        );
        return;
    }
    let plist = plist.canonicalize().unwrap_or(plist);
    println!(
        "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}
