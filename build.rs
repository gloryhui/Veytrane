use std::{env, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    // A distributor supplies a private build-time profile. Never emit its
    // contents in cargo directives, diagnostics, source control or the ZIP.
    println!("cargo:rerun-if-env-changed=P2P_SUB2API_PROFILE_FILE");
    let profile = if env::var_os("CARGO_FEATURE_SUB2API_CLIENT").is_some() {
        env::var_os("P2P_SUB2API_PROFILE_FILE")
            .map(|path| {
                println!(
                    "cargo:rerun-if-changed={}",
                    std::path::Path::new(&path).display()
                );
                std::fs::read_to_string(path).expect("read private Sub2API build profile")
            })
            .unwrap_or_else(|| "{}".into())
    } else {
        "{}".into()
    };
    std::fs::write(
        std::path::Path::new(&env::var_os("OUT_DIR").unwrap()).join("sub2api-profile.json"),
        profile,
    )
    .expect("write embedded Sub2API profile");
    println!("cargo:rerun-if-changed=assets/icons/app-icon.ico");
    println!("cargo:rerun-if-changed=assets/icons/app-icon.rc");
    if env::var_os("CARGO_FEATURE_GUI").is_some()
        && env::var("TARGET").is_ok_and(|target| target.contains("windows"))
    {
        embed_resource::compile_for(
            "assets/icons/app-icon.rc",
            ["p2p-desktop"],
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("compile Windows application icon");
    }
    println!("cargo:rerun-if-changed=build.rs");
    // Worktrees use a .git file. Watch the actual HEAD, index and branch ref so
    // freezing a candidate rebuilds metadata even when Rust source is unchanged.
    for name in ["HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(files) = git(&["ls-files"]) {
        for path in files.lines() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let sha = git(&["rev-parse", "HEAD"])
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    let state = git(&["status", "--porcelain"])
        .map(|s| if s.is_empty() { "clean" } else { "dirty" })
        .unwrap_or("unknown");
    println!("cargo:rustc-env=P2P_BUILD_SHA={sha}");
    println!("cargo:rustc-env=P2P_BUILD_STATE={state}");
    println!(
        "cargo:rustc-env=P2P_BUILD_TARGET={}",
        env::var("TARGET").unwrap()
    );
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
}
