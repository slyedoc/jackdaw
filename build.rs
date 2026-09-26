// RPATH for libshaderc_shared.so.1: bevy_aurora links shaderc from the Vulkan SDK, and the
// SDK's setup-env.sh puts only $VULKAN_SDK/lib/VulkanLoader/lib on LD_LIBRARY_PATH -- not
// $VULKAN_SDK/lib, where shaderc actually lives. `cargo:rustc-link-arg` does NOT propagate
// from a dependency's build script, so every binary in a downstream workspace embeds the
// rpath itself. Without this the editor and its test binaries LINK fine and then die at
// startup with "error while loading shared libraries". Same pattern as aurora_files' bsn.
//
// --disable-new-dtags emits DT_RPATH, which applies to every lookup in the process. The
// default DT_RUNPATH would not reach libjackdaw_dylib.so, which is what needs shaderc.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    if let Ok(sdk) = std::env::var("VULKAN_SDK") {
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{sdk}/lib");
    }
}
