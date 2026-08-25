//! Кладёт VERSIONINFO в собранную DLL, чтобы данные пакета были видны
//! в свойствах файла, а не только в Cargo.toml.

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let version = env!("CARGO_PKG_VERSION");
    let authors = env!("CARGO_PKG_AUTHORS");
    let description = env!("CARGO_PKG_DESCRIPTION");
    let repository = env!("CARGO_PKG_REPOSITORY");

    let mut resource = winresource::WindowsResource::new();
    resource
        .set("ProductName", "piscatio")
        .set("FileDescription", description)
        .set("CompanyName", authors)
        .set("LegalCopyright", "MIT. (c) Genius Loci")
        .set("InternalName", "piscatio")
        .set("OriginalFilename", "piscatio.dll")
        .set("Comments", &format!("{description} — {repository}"))
        .set("FileVersion", version)
        .set("ProductVersion", version);

    // Отсутствие rc.exe не должно ломать сборку: без ресурса DLL
    // всё равно рабочая, просто без данных в свойствах файла.
    if let Err(e) = resource.compile() {
        println!("cargo:warning=VERSIONINFO не собран ({e}), DLL будет без свойств файла");
    }
}
