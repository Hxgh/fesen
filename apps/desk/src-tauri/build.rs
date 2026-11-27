fn main() {
    #[cfg(target_os = "macos")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let host = "../native/macos/Host.swift";
        let accessory = "../native/macos/Accessory.swift";
        let preference = "../native/macos/MenuBarPreference.swift";
        let physical = "../../core/native/macos/PhysicalNetwork.swift";
        let header = "../../core/native/macos/Core.h";
        for input in [host, accessory, preference, physical, header] {
            println!("cargo:rerun-if-changed={input}");
        }
        let out =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("fesen-host.o");
        assert_eq!(
            std::env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
            Ok("aarch64"),
            "macOS only targets Apple Silicon (arm64)"
        );
        let status = std::process::Command::new("swiftc")
            .args([
                "-swift-version",
                "5",
                "-target",
                "arm64-apple-macos15.0",
                "-O",
                "-whole-module-optimization",
                "-parse-as-library",
                "-import-objc-header",
                header,
                "-emit-object",
                host,
                accessory,
                preference,
                physical,
                "-o",
            ])
            .arg(&out)
            .status()
            .expect("swiftc is required for macOS");
        assert!(
            status.success(),
            "the Network Extension host did not compile"
        );
        let library = out.with_file_name("libfesen_host.a");
        assert!(
            std::process::Command::new("xcrun")
                .args(["libtool", "-static", "-o"])
                .arg(&library)
                .arg(&out)
                .status()
                .expect("could not compile macOS host")
                .success()
        );
        println!(
            "cargo:rustc-link-search=native={}",
            out.parent().unwrap().display()
        );
        println!("cargo:rustc-link-lib=static=fesen_host");
        // The Swift object carries its system-framework autolink declarations.
        println!("cargo:rustc-link-search=native=/usr/lib/swift");
    }
    tauri_build::build();
}
