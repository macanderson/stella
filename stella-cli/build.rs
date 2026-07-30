use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=STELLA_BUILD_GIT_SHA");

    let package_version =
        env::var("CARGO_PKG_VERSION").expect("Cargo must provide CARGO_PKG_VERSION");
    let build_version = match env::var("STELLA_BUILD_GIT_SHA") {
        Ok(sha) if !sha.is_empty() => {
            assert!(
                sha.is_ascii() && !sha.bytes().any(|byte| matches!(byte, b'\r' | b'\n')),
                "STELLA_BUILD_GIT_SHA must be ASCII and must not contain a newline"
            );
            format!("{package_version}-dev.{sha}")
        }
        _ => package_version,
    };

    println!("cargo:rustc-env=STELLA_BUILD_VERSION={build_version}");

    // Build identity for the long `--version`. A bug report that says only
    // "stella 0.6.8" leaves the two questions that actually matter — which
    // machine was this built for, and was it a debug build? — to a round trip.
    // Cargo always provides both to a build script, so neither can be absent;
    // the fallbacks exist so a `build.rs` invoked outside cargo cannot panic
    // the build over a cosmetic string.
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=STELLA_BUILD_TARGET={target}");
    println!("cargo:rustc-env=STELLA_BUILD_PROFILE={profile}");
}
