use std::{env, fmt::Write as _, fs, path::PathBuf};

use heck::ToUpperCamelCase;
use prost_reflect::{ReflectMessage, Value};

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=proto");
    let mut compiler = protox::Compiler::new(["proto"])?;
    compiler.include_imports(true);
    compiler.open_file("api.proto")?;
    let fds = compiler.file_descriptor_set();
    let pool = compiler.descriptor_pool();

    prost_build::Config::new().bytes(["."]).compile_fds(fds)?;

    // Message ids live in the `(id)` option (extension 1036 on MessageOptions).
    let mut ids: Vec<(u32, String)> = Vec::new();
    for msg in pool.all_messages() {
        let opts = msg.options();
        let Some(ext) = opts
            .descriptor()
            .extensions()
            .find(|e| e.full_name() == "id")
        else {
            continue;
        };
        if !opts.has_extension(&ext) {
            continue;
        }
        if let Value::U32(id) = *opts.get_extension(&ext)
            && id != 0
        {
            ids.push((id, msg.name().to_owned()));
        }
    }
    ids.sort();

    let mut out = String::new();
    for (id, name) in &ids {
        let ty = name.to_upper_camel_case();
        writeln!(
            out,
            "impl ApiMessage for proto::{ty} {{ const ID: u32 = {id}; }}"
        )?;
    }
    out.push_str("/// `(id, message name)` for every message that has a wire id, sorted by id.\n");
    out.push_str("pub const MESSAGE_TABLE: &[(u32, &str)] = &[\n");
    for (id, name) in &ids {
        writeln!(out, "    ({id}, \"{name}\"),")?;
    }
    out.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR")?).join("ids.rs"), out)?;
    Ok(())
}
