//! Static, resource-free WGSL sketches for an inline 256 × 256 WebGL2 canvas.

use naga::back::glsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{Block, Function, ShaderStage, Statement, TypeInner};
use serde::Serialize;

use super::IpcFailure;

const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_FUNCTIONS: usize = 32;
const MAX_EXPRESSIONS: usize = 4096;
const MAX_EXPANDED_WORK: usize = 1024;
const ENTRY_NAME: &str = "loom_preview";
const ENTRY: &str = "\n@fragment fn loom_preview(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> { return shade(pos.xy / vec2<f32>(256.0, 256.0)); }\n";
static COMPILER: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[derive(Debug, Serialize)]
pub(super) struct ShaderPreview {
    pub fragment: String,
}

#[tauri::command]
pub(super) async fn shader_preview(source: String) -> Result<ShaderPreview, IpcFailure> {
    validate_source(&source)?;
    let permit = COMPILER.try_acquire().map_err(|_| {
        IpcFailure::new(
            "shader_preview_busy",
            "Another sketch is compiling. Try again shortly.",
            true,
        )
    })?;
    // Keep the permit inside the blocking worker: dropping the IPC future does
    // not cancel CPU work or admit overlapping compilers.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        compile(&source)
    })
    .await
    .map_err(|_| failure("The shader compiler did not complete."))?
}

fn compile(source: &str) -> Result<ShaderPreview, IpcFailure> {
    validate_source(source)?;
    let combined = format!("{source}{ENTRY}");
    let module = naga::front::wgsl::parse_str(&combined)
        .map_err(|error| failure(error.emit_to_string(&combined)))?;
    if module.entry_points.len() != 1
        || module.entry_points[0].name != ENTRY_NAME
        || module.entry_points[0].stage != ShaderStage::Fragment
        || !module.global_variables.is_empty()
        || !module.overrides.is_empty()
    {
        return Err(failure(
            "Provide ordinary functions only, without entry points, global variables, or resource bindings.",
        ));
    }
    if module.functions.len() > MAX_FUNCTIONS
        || module.global_expressions.len()
            + module
                .functions
                .iter()
                .map(|(_, function)| function.expressions.len())
                .sum::<usize>()
            > MAX_EXPRESSIONS
    {
        return Err(failure(
            "This sketch exceeds the expression or function budget.",
        ));
    }
    if module.types.iter().any(|(_, ty)| {
        !matches!(
            ty.inner,
            TypeInner::Scalar(_) | TypeInner::Vector { .. } | TypeInner::Matrix { .. }
        )
    }) {
        return Err(failure(
            "Sketches support scalar, vector, and matrix values; arrays, structs, pointers, and resources are unsupported.",
        ));
    }
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|error| failure(error.emit_to_string(&combined)))?;

    // Naga orders every callee before its callers and rejects recursion. Count
    // expanded calls, not just source statements, to reject exponential work.
    let mut costs = Vec::with_capacity(module.functions.len());
    for (_, function) in module.functions.iter() {
        costs.push(function_cost(function, &costs)?);
    }
    function_cost(&module.entry_points[0].function, &costs)?;

    let options = glsl::Options {
        version: glsl::Version::Embedded {
            version: 300,
            is_webgl: true,
        },
        ..Default::default()
    };
    let pipeline = glsl::PipelineOptions {
        shader_stage: ShaderStage::Fragment,
        entry_point: ENTRY_NAME.to_owned(),
        multiview: None,
    };
    let mut fragment = String::new();
    glsl::Writer::new(
        &mut fragment,
        &module,
        &info,
        &options,
        &pipeline,
        naga::proc::BoundsCheckPolicies::default(),
    )
    .map_err(|error| failure(error.to_string()))?
    .write()
    .map_err(|error| failure(error.to_string()))?;
    if fragment.len() > MAX_OUTPUT_BYTES {
        return Err(failure("The compiled sketch exceeds 64 KiB."));
    }
    Ok(ShaderPreview { fragment })
}

fn function_cost(function: &Function, callees: &[usize]) -> Result<usize, IpcFailure> {
    let mut cost = function.expressions.len() + function.local_variables.len();
    block_cost(&function.body, callees, 0, &mut cost)?;
    check_cost(cost)
}

fn block_cost(
    block: &Block,
    callees: &[usize],
    depth: usize,
    cost: &mut usize,
) -> Result<(), IpcFailure> {
    if depth > MAX_DEPTH {
        return Err(failure("This sketch is nested too deeply."));
    }
    for statement in block {
        *cost += 1;
        match statement {
            Statement::Block(body) => block_cost(body, callees, depth + 1, cost)?,
            Statement::If { accept, reject, .. } => {
                block_cost(accept, callees, depth + 1, cost)?;
                block_cost(reject, callees, depth + 1, cost)?;
            }
            Statement::Switch { cases, .. } => {
                for case in cases {
                    block_cost(&case.body, callees, depth + 1, cost)?;
                }
            }
            Statement::Call { function, .. } => {
                *cost += callees.get(function.index()).copied().ok_or_else(|| {
                    failure("Recursive or unresolved shader calls are unsupported.")
                })?;
            }
            Statement::Emit(_) | Statement::Return { .. } | Statement::Store { .. } => {}
            _ => {
                return Err(failure(
                    "Sketches may use ordinary math and conditionals, but not loops, discard, synchronization, or resource operations.",
                ));
            }
        }
        check_cost(*cost)?;
    }
    Ok(())
}

fn check_cost(cost: usize) -> Result<usize, IpcFailure> {
    if cost > MAX_EXPANDED_WORK {
        Err(failure(
            "This sketch exceeds the per-pixel work budget. Simplify its expressions or helper calls.",
        ))
    } else {
        Ok(cost)
    }
}

/// Bound nesting before the recursive parser, ignoring ordinary WGSL comments.
/// Array types are excluded before constant evaluation can materialize them.
fn validate_source(source: &str) -> Result<(), IpcFailure> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(failure("A sketch can contain at most 16 KiB of WGSL."));
    }
    let bytes = source.as_bytes();
    let mut offset = 0;
    let mut depth = 0_usize;
    let mut comments = 0_usize;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.starts_with(b"/*") {
            comments += 1;
            if comments > MAX_DEPTH {
                return Err(failure("Shader comments are nested too deeply."));
            }
            offset += 2;
        } else if comments > 0 {
            if remaining.starts_with(b"*/") {
                comments -= 1;
                offset += 2;
            } else {
                offset += 1;
            }
        } else if remaining.starts_with(b"//") {
            offset += remaining
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(remaining.len());
        } else if bytes[offset].is_ascii_alphabetic() || bytes[offset] == b'_' {
            let start = offset;
            while offset < bytes.len()
                && (bytes[offset].is_ascii_alphanumeric() || bytes[offset] == b'_')
            {
                offset += 1;
            }
            if matches!(
                &bytes[start..offset],
                b"array" | b"binding_array" | b"atomic"
            ) {
                return Err(failure(
                    "Arrays and atomic values are unsupported in inline sketches.",
                ));
            }
        } else {
            match bytes[offset] {
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err(failure("This sketch is nested too deeply."));
                    }
                }
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            offset += 1;
        }
    }
    Ok(())
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("shader_preview_invalid", message, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRADIENT: &str = "fn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(p, 0.5, 1.0); }";

    #[test]
    fn gradient_compiles_to_webgl2_fragment_shader() {
        let preview = compile(GRADIENT).expect("valid gradient");
        assert!(preview.fragment.starts_with("#version 300 es"));
        assert!(preview.fragment.contains("void main()"));
        assert!(preview.fragment.contains("gl_FragCoord"));
    }

    #[test]
    fn loops_are_rejected_even_in_unused_helpers() {
        for helper in [
            "fn hidden() { loop { break; } }",
            "fn hidden() { for(var i = 0; i < 3; i++) {} }",
            "fn hidden() { if true { while true {} } }",
        ] {
            assert!(
                compile(&format!("{GRADIENT}\n{helper}")).is_err(),
                "{helper}"
            );
        }
    }

    #[test]
    fn caller_entry_points_resources_and_wrong_interfaces_are_rejected() {
        for source in [
            "fn shade(p: f32) -> vec4<f32> { return vec4<f32>(p); }",
            "fn shade(p: vec2<f32>) -> f32 { return p.x; }",
            "@compute @workgroup_size(1) fn own_entry() {}",
            "@group(0) @binding(0) var<uniform> secret: vec4<f32>;",
            "var<private> state: f32;",
        ] {
            let input = if source.starts_with("fn shade") {
                source.to_owned()
            } else {
                format!("{GRADIENT}\n{source}")
            };
            assert!(compile(&input).is_err(), "{source}");
        }
    }

    #[test]
    fn bounded_source_does_not_admit_exponential_helper_calls() {
        let mut source = String::from("fn f0(p: vec2<f32>) -> vec2<f32> { return p * 0.5; }\n");
        for i in 1..12 {
            use std::fmt::Write as _;
            writeln!(
                source,
                "fn f{i}(p: vec2<f32>) -> vec2<f32> {{ return f{}(p) + f{}(p); }}",
                i - 1,
                i - 1
            )
            .expect("write shader source");
        }
        source.push_str(
            "fn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(f11(p), 0.0, 1.0); }",
        );
        let error = compile(&source).expect_err("expanded cost must be bounded");
        assert!(error.message.contains("work budget"));
    }

    #[test]
    fn excessive_input_nesting_and_array_allocations_fail_before_compilation() {
        assert!(compile(&" ".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        assert!(compile(&format!("{}{}", "(".repeat(MAX_DEPTH + 1), GRADIENT)).is_err());
        assert!(compile("const huge = array<u32, 1000000000>();").is_err());
        assert!(compile(&format!("/* array ((( */\n// array {{{{\n{GRADIENT}")).is_ok());
    }
}
