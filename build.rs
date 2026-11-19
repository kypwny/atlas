//! Windows: the logo as the program's file icon and a proper name in the file properties.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo/atlas.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/logo/atlas.ico");
        res.set("ProductName", "Atlas");
        res.set("FileDescription", "Atlas - Call of Duty Asset Workbench");
        if let Err(e) = res.compile() {
            // a build without the Windows SDK still works, just without the file icon
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
