#![cfg(feature = "gpu")]
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CaptureIO, CpuRuntime, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

#[test]
fn unbound_copy_retains_its_snapshot_after_the_original_gpu_slot_is_repainted() {
    let source = r#"
@{group 0 binding 0} (var painted: (Texture2D f32))
@{group 0 binding 1} (var restored: (Texture2D f32))
@{group 0 binding 2 address storage-write} (var sampled: vec4f)
(var retained: (Texture2D f32))
@vertex (defn full []: @{builtin position} vec4f
  (vec4f (match (vertex-index) 0u (vec2f -1. -1.) 1u (vec2f 3. -1.) _ (vec2f -1. 3.)) 0. 1.))
@fragment (defn red []: @{location 0} vec4f (vec4f 1. 0. 0. 1.))
@fragment (defn green []: @{location 0} vec4f (vec4f 0. 1. 0. 1.))
@cpu (defn main []
  (= painted (blank-texture 2u 2u))
  (spawn-window (fn []
    (set-render-target painted) (dispatch-render-shaders full red 3u) (clear-render-target)
    (= retained painted)
    (set-render-target painted) (dispatch-render-shaders full green 3u) (clear-render-target)
    (= restored retained)
    (dispatch-compute-shader (fn [] (= sampled (texture-load restored (vec2u 0u) 0u))) (vec3u 1u))
    (print sampled)
    (dispatch-compute-shader (fn [] (= sampled (texture-load painted (vec2u 0u) 0u))) (vec3u 1u))
    (print sampled) (close-window))))
"#;
    let docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "staging.easl".into(),
        source.into(),
    );
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let (io, _) = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            CaptureIO::new(),
            Path::new("staging.easl"),
            runtime,
        )
        .unwrap();
        assert_eq!(
            io.prints,
            ["(vec4f 1. 0. 0. 1.)", "(vec4f 0. 1. 0. 1.)"],
            "{runtime:?}"
        );
    }
}
