//! Measure EASL width-only composition and geometry separately from font loading
//! and painting. This is a component benchmark, not a native input latency test.
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    interpreter::{StringIO, VmCpuRuntime},
    parse::load_and_parse_easl_multidocument,
};
use std::{path::Path, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/flow_specimen.easl");
    let documents = load_and_parse_easl_multidocument(&path)?
        .map_err(|_| "Could not parse imports")?
        .map_err(|(_, errors)| format!("{errors:?}"))?;
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
        StringIO::new(),
        path.parent().map(Path::to_path_buf),
        Some(external.clone()),
    )?;
    let compile = start.elapsed();
    let start = Instant::now();
    vm.run("flow-prepare")?;
    let prepare = start.elapsed();
    if external.read_external_var_raw("flow-status")? != [0] {
        return Err("Initial flow failed".into());
    }
    let mut samples = Vec::with_capacity(100);
    for i in 0..105 {
        let width: f32 = if i % 2 == 0 { 410. } else { 820. };
        external.write_external_var_raw("flow-width", &[width.to_bits()])?;
        let start = Instant::now();
        vm.run("flow-reflow")?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.;
        if external.read_external_var_raw("flow-status")? != [0] {
            return Err("Width-only reflow failed".into());
        }
        if i >= 5 {
            samples.push(elapsed);
        }
    }
    if external.read_external_var_raw("flow-shapes")? != [1] {
        return Err("Width-only reflow repeated shaping".into());
    }
    vm.run("flow-dispose")?;
    if vm.env.text_values.live_values() != 0 {
        return Err("Flow consumer leaked native text values".into());
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "compile_ms={:.3} prepare_ms={:.3} width_reflow_samples={} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} shaping_passes=1 retained_values=0",
        compile.as_secs_f64() * 1000.,
        prepare.as_secs_f64() * 1000.,
        samples.len(),
        samples[49],
        samples[94],
        samples[98]
    );
    Ok(())
}
