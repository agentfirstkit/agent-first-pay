fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "rpc")]
    tonic_prost_build::compile_protos("proto/afpay.proto")?;
    Ok(())
}
