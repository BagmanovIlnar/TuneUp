fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || !std::env::var("HOST")
            .unwrap_or_default()
            .contains("windows")
    {
        println!("cargo:warning=helper manifest embedding requires a Windows build host");
        return;
    }
    let manifest = r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;
    let mut resource = winres::WindowsResource::new();
    resource.set_manifest(manifest);
    resource.compile().expect("failed to embed helper manifest");
}
