fn main() {
    println!("cargo:rerun-if-changed=vitrobar.rc");
    println!("cargo:rerun-if-changed=vitrobar.exe.manifest");
    println!("cargo:rerun-if-changed=vitrobar.ico");
    embed_resource::compile("vitrobar.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
