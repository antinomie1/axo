//! Compiles the WGSL shaders to SPIR-V at build time; no compiler ships at run time.

use std::{env, fs, path::PathBuf};

use naga::{
    ShaderStage,
    back::spv::{self, Options, PipelineOptions, WriterFlags},
    valid::{Capabilities, ValidationFlags, Validator},
};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let path = "shaders/terminal.wgsl";
    println!("cargo:rerun-if-changed={path}");
    let source = fs::read_to_string(path).expect("read shader");
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|error| panic!("{path}: {}", error.emit_to_string(&source)));
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|error| panic!("{path}: {}", error.emit_to_string(&source)));
    let options = Options {
        lang_version: (1, 3),
        flags: WriterFlags::empty(),
        fake_missing_bindings: true,
        ..Options::default()
    };
    for (suffix, shader_stage, entry_point) in [
        ("vert", ShaderStage::Vertex, "vs_main"),
        ("frag", ShaderStage::Fragment, "fs_main"),
    ] {
        let words = spv::write_vec(
            &module,
            &info,
            &options,
            Some(&PipelineOptions {
                shader_stage,
                entry_point: entry_point.into(),
            }),
        )
        .unwrap_or_else(|error| panic!("{path}/{entry_point}: {error}"));
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        fs::write(out.join(format!("terminal.{suffix}.spv")), bytes).expect("write SPIR-V");
    }
}
