//! Native EASL GPU regressions; no application or editor host.
#![cfg(feature = "gpu")]

const PAINT: &str = r#"
@{group 0 binding 0 address uniform} (var paint-color: vec4f)
@{group 0 binding 1 address uniform} (var paint-rect: vec4f)
@vertex
(defn paint-vertex []: @{builtin position} vec4f
  (let [corner (match (vertex-index) 0u (vec2f 0. 0.) 1u (vec2f 1. 0.)
      2u (vec2f 0. 1.) 3u (vec2f 0. 1.) 4u (vec2f 1. 0.) _ (vec2f 1. 1.))]
    (vec4f (+ paint-rect.xy (* paint-rect.zw corner)) 0. 1.)))
@fragment
(defn paint-fragment []: @{location 0} vec4f paint-color)
@cpu
(defn paint []
  (= paint-rect (vec4f -1. -1. 1. 2.)) (= paint-color (vec4f 1. 0. 0. 1.))
  (dispatch-render-shaders paint-vertex paint-fragment 6u)
  (= paint-rect (vec4f 0. -1. 1. 2.)) (= paint-color (vec4f 0. 0. 1. 1.))
  (dispatch-render-shaders paint-vertex paint-fragment 6u))
@cpu (defn main [] (spawn-window paint))
"#;

fn with_body(declarations: &str, body: &str) -> String {
    format!(
        "{declarations}\n{}\n@cpu (defn main [] (spawn-window (fn [] {body})))",
        PAINT.split("@cpu (defn main").next().unwrap()
    )
}

#[test]
fn retained_native_gpu_corpus_keeps_order_in_both_easl_evaluators() {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        interpreter::{CaptureIO, CpuRuntime, run_program_entry_with_io_and_runtime_from_path},
        parse::load_and_parse_easl_multidocument,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/easl/data/buffer");
    for name in [
        "offscreen_render_compute_order",
        "render_target_pingpong",
        "bidirectional_transfer_render",
    ] {
        let path = root.join(format!("{name}.easl"));
        let expected = std::fs::read_to_string(root.join(format!("{name}.txt"))).unwrap();
        let docs = load_and_parse_easl_multidocument(&path)
            .unwrap()
            .unwrap()
            .unwrap();
        let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(errors.is_empty(), "{name}: {errors:?}");
        for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
            let (io, _) = run_program_entry_with_io_and_runtime_from_path(
                program.clone(),
                Some("main"),
                CaptureIO::new(),
                &path,
                runtime,
            )
            .unwrap();
            assert_eq!(io.prints.join("\n"), expected.trim(), "{name}");
        }
    }
}

#[test]
fn texture_copies_snapshot_gpu_ink_and_rebind_render_targets_in_both_evaluators() {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        interpreter::{CaptureIO, CpuRuntime, run_program_entry_with_io_and_runtime_from_path},
        parse::{EaslMultiDocument, parse_easl_without_comments},
    };
    let source = with_body(
        r#"
        @{group 1 binding 0} (var sheet: (Texture2D f32))
        @{group 1 binding 1} (var cached: (Texture2D f32))
        @{group 1 binding 2 address storage-write} (var observed: vec4f)
        "#,
        r#"
        (= sheet (blank-texture (vec2u 8u 4u)))
        (set-render-target sheet) (paint) (clear-render-target)
        (= cached sheet) (= cached cached)
        ; Reading the copy must see completed red/blue GPU writes, not blank CPU data.
        (dispatch-compute-shader (fn [] (= observed (texture-load cached (vec2u 1u) 0u))) (vec3u 1u))
        (print observed)
        (dispatch-compute-shader (fn [] (= observed (texture-load cached (vec2u 6u 1u) 0u))) (vec3u 1u))
        (print observed)
        ; Drawing into the copy must target its binding, leaving the source intact.
        (set-render-target cached)
        (= paint-rect (vec4f -1. -1. 2. 2.)) (= paint-color (vec4f 0. 1. 0. 1.))
        (dispatch-render-shaders paint-vertex paint-fragment 6u)
        (clear-render-target)
        (dispatch-compute-shader (fn [] (= observed (texture-load sheet (vec2u 1u) 0u))) (vec3u 1u))
        (print observed)
        (dispatch-compute-shader (fn [] (= observed (texture-load cached (vec2u 1u) 0u))) (vec3u 1u))
        (print observed)
        (close-window)
        "#,
    );
    let docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(&source),
        "texture-copy.easl".into(),
        source,
    );
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        program
            .validate_raw_program(CompilerTarget::WGSL)
            .is_empty()
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("texture-copy.easl");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let (io, _) = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            CaptureIO::new(),
            &path,
            runtime,
        )
        .unwrap();
        assert_eq!(
            io.prints,
            [
                "(vec4f 1. 0. 0. 1.)",
                "(vec4f 0. 0. 1. 1.)",
                "(vec4f 1. 0. 0. 1.)",
                "(vec4f 0. 1. 0. 1.)",
            ]
        );
    }
}
