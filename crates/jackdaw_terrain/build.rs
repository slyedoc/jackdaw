// See the root build.rs: bevy_aurora links libshaderc_shared.so.1 from the Vulkan SDK, and
// `cargo:rustc-link-arg` does not propagate from a dependency's build script, so every
// package that produces a RUNNABLE artifact against aurora embeds the rpath itself.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    if let Ok(sdk) = std::env::var("VULKAN_SDK") {
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{sdk}/lib");
    }
}
