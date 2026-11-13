fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a vendored protoc so no system install is needed (local, CI and Docker builds).
    if std::env::var("PROTOC").is_err() {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/lkube.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/lkube.proto");
    Ok(())
}
