//! Run the ordinary EASL font specimen on a headless GPU. All text placement,
//! vertex generation and fragment shading are in the imported EASL library.
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    interpreter::{CaptureIO, VmCpuRuntime},
    parse::{load_and_parse_easl_multidocument, parse_easl_without_comments},
};
use std::{path::PathBuf, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("Pass an output PNG path")?;
    let output = std::path::absolute(output)?;
    let output_text = output.to_str().ok_or("Output path must be UTF-8")?;
    if output_text.contains('"') {
        return Err("The EASL output-path literal cannot contain a double quote".into());
    }
    // EASL literals preserve backslashes; Rust's Debug escaping would change paths.
    let output_literal = format!("\"{output_text}\"");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_path = std::env::args_os()
        .nth(2)
        .map_or_else(|| root.join("examples/font_atlas.easl"), PathBuf::from);
    let entry = std::env::args().nth(3).unwrap_or_else(|| "main".into());
    let mut documents = load_and_parse_easl_multidocument(&source_path)?
        .map_err(|_| "Could not parse EASL specimen imports")?
        .map_err(|(_, errors)| format!("{errors:?}"))?;
    for (parsed, _, source) in &mut documents.sources {
        *source = source
            .replace("\"font-atlas-specimen.png\"", &output_literal)
            .replace("\"editor-specimen.png\"", &output_literal)
            .replace("\"view-specimen.png\"", &output_literal)
            .replace("\"flow-specimen.png\"", &output_literal)
            .replace("\"style-specimen.png\"", &output_literal);
        *parsed = parse_easl_without_comments(source);
        if !parsed.parsing_failures.is_empty() {
            return Err(format!("{:?}", parsed.parsing_failures).into());
        }
    }
    let start = Instant::now();
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    if !errors.is_empty() {
        return Err(format!("{errors:?}").into());
    }
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    if !errors.is_empty() {
        return Err(format!("{errors:?}").into());
    }
    let external = ExternalVars::new(&program);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        CaptureIO::new(),
        source_path.parent().map(PathBuf::from),
        Some(external.clone()),
    )?;
    let compiled = start.elapsed();
    let start = Instant::now();
    vm.run(&entry)?;
    if entry == "editor-proof" {
        if external.read_external_var_raw("proof-history-roundtrip")? != vec![1] {
            return Err("EASL editor did not retain exact undo/redo source".into());
        }
        if vm.env.text_values.live_values() != 0 {
            return Err("EASL editor leaked retained text values".into());
        }
    }
    if matches!(
        entry.as_str(),
        "view-selection-proof" | "view-preedit-proof" | "flow-render-proof" | "flow-scroll-proof"
    ) {
        if external.read_external_var_raw("demo-proof-ok")? != vec![1] {
            return Err("EASL view input did not preserve the expected selection/source".into());
        }
        if vm.env.text_values.live_values() != 0 {
            return Err("EASL view leaked retained text values".into());
        }
    }
    println!(
        "EASL compile: {compiled:?}; font preparation and headless GPU render: {:?}; {}",
        start.elapsed(),
        output.display()
    );
    Ok(())
}
