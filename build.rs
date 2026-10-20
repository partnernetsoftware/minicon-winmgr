fn main() {
    println!("cargo:rerun-if-env-changed=MINICON_TSKMGR_SHA256");
    if let Ok(digest) = std::env::var("MINICON_TSKMGR_SHA256") {
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "MINICON_TSKMGR_SHA256 must be a lowercase SHA-256 digest"
        );
        println!("cargo:rustc-env=MINICON_TSKMGR_SHA256={digest}");
    }
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("app.manifest");
        println!("cargo:rustc-link-arg-bin=minicon-winmgr=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=minicon-winmgr=/MANIFESTINPUT:{}",
            manifest.display()
        );
        let task_manifest =
            std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
                .join("tskmgr.manifest");
        println!("cargo:rerun-if-changed=tskmgr.manifest");
        println!("cargo:rustc-link-arg-bin=minicon-tskmgr=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=minicon-tskmgr=/MANIFESTINPUT:{}",
            task_manifest.display()
        );
    }
}
