fn main() -> Result<(), Box<dyn std::error::Error>> {
    // prost-build locates protoc via the PROTOC env var; point it at the
    // vendored binary so builds don't depend on a system install.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    tonic_build::configure().compile_protos(&["../../proto/ni/v1/ni.proto"], &["../../proto"])?;
    Ok(())
}
