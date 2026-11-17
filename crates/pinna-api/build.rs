use std::{env, fs, path::PathBuf};

use prost::Message as _;

const FILES: &[&str] = &["pinna/v1/device.proto", "pinna/v1/assistant.proto"];

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=../../proto");
    let mut compiler = protox::Compiler::new(["../../proto"])?;
    compiler.include_imports(true);
    compiler.include_source_info(true);
    for file in FILES {
        compiler.open_file(file)?;
    }
    let fds = PathBuf::from(env::var("OUT_DIR")?).join("pinna.fds");
    fs::write(&fds, compiler.file_descriptor_set().encode_to_vec())?;

    connectrpc_build::Config::new()
        .files(FILES)
        .includes(&["../../proto"])
        .descriptor_set(fds)
        .include_file("_connectrpc.rs")
        .compile()?;
    Ok(())
}
