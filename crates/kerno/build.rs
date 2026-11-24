//! Windows: the executable's icon, compiled into it as a resource
//! (`assets/icon/kerno.rc`, K66). Nothing to do on the other systems.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../assets/icon/kerno.rc");
        println!("cargo:rerun-if-changed=../../assets/icon/kerno.ico");
        // GPUI's own resource file brings the manifest; this one brings the icon.
        if let Err(error) =
            embed_resource::compile("../../assets/icon/kerno.rc", embed_resource::NONE)
                .manifest_required()
        {
            panic!("the icon of kerno.exe could not be compiled in: {error}");
        }
    }
}
