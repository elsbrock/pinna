use std::{env, fs, path::PathBuf};

use prost::Message as _;

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=../../proto");
    let mut compiler = protox::Compiler::new(["../../proto"])?;
    compiler.include_imports(true);
    compiler.include_source_info(true);
    compiler.open_file("pinna/v1/device.proto")?;
    let fds = PathBuf::from(env::var("OUT_DIR")?).join("pinna.fds");
    fs::write(&fds, compiler.file_descriptor_set().encode_to_vec())?;

    connectrpc_build::Config::new()
        .files(&["pinna/v1/device.proto"])
        .includes(&["../../proto"])
        .descriptor_set(fds)
        .include_file("_connectrpc.rs")
        .compile()?;
    Ok(())
}
