fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = slint_build::CompilerConfiguration::new();
    let config = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        // The mobile software renderer does not build Slint's runtime image decoders.
        Ok("android" | "ios") => {
            config.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
        }
        _ => config,
    };
    slint_build::compile_with_config("ui/main.slint", config)?;
    Ok(())
}
