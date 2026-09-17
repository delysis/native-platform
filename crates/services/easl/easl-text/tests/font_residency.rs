use easl::{
    CompilerTarget,
    compiler::{
        builtins::built_in_macros, expression::ExpKind, functions::FunctionImplementationKind,
        program::Program,
    },
    interpreter::{BufferUpload, EvaluationEnvironment, StringIO, VmCpuRuntime, eval},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

const SOURCE: &str = r#"
@{group 0 binding 0} (var page: (Texture2D f32))
@{group 0 binding 1} (var bound: (Texture2D f32))
(var ids: [u32]) (var glyphs: [FontShapedGlyph]) (var source: TextBuffer)
@cpu (defn setup []
  (= page (open-font "../easl-native-text/tests/fonts/amiri.ttf" 256.))
  (= source (make-text "Heading"))
  (= glyphs (shape-font-span page source 0u 7u 1818326126u false))
  (= ids (zeroed-array (array-length glyphs)))
  (for [i (array-length glyphs)] (= (ids i) (.id (glyphs i))))
  (= page (rasterize-font page ids)) (= bound page) (text-release source))
@cpu (defn swap [] (= bound page))
@cpu (defn clear [] (= bound (blank-texture 1u 1u)))
"#;

fn program(source: &str) -> Program {
    let docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "font-residency.easl".into(),
        source.into(),
    );
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        program
            .validate_raw_program(CompilerTarget::WGSL)
            .is_empty()
    );
    program
}

fn bitmap(uploads: &[((u8, u8), BufferUpload)], binding: u8) -> &[u8] {
    let (_, BufferUpload::TextureData { data, .. }) = uploads
        .iter()
        .find(|(key, _)| *key == (0, binding))
        .unwrap()
    else {
        panic!("expected texture")
    };
    data
}

#[test]
fn retained_font_page_assignment_and_dispatch_snapshots_share_bitmap_storage() {
    let program = program(SOURCE);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program.clone(),
        StringIO::new(),
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).into()),
        None,
    )
    .unwrap();
    vm.run("setup").unwrap();
    let first = vm.env.binding_buffer_data();
    vm.run("swap").unwrap();
    let second = vm.env.binding_buffer_data();
    vm.run("clear").unwrap();
    assert_shared_storage(&first, &second, &vm.env.binding_buffer_data());
    let mut tree = EvaluationEnvironment::from_program(
        program.clone(),
        StringIO::new(),
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).into()),
    )
    .unwrap();
    let run = |name: &str, tree: &mut EvaluationEnvironment<StringIO>| {
        let entries = program.cpu_entry_points();
        let entry = entries
            .iter()
            .find(|f| &*f.read().unwrap().name == name)
            .unwrap()
            .read()
            .unwrap();
        let FunctionImplementationKind::Composite(function) = &entry.implementation else {
            panic!("expected body")
        };
        let function = function.read().unwrap();
        let ExpKind::Function(_, body) = &function.expression.kind else {
            panic!("expected function")
        };
        eval(*body.clone(), tree).unwrap();
    };
    run("setup", &mut tree);
    let first = tree.binding_buffer_data();
    run("swap", &mut tree);
    let second = tree.binding_buffer_data();
    run("clear", &mut tree);
    assert_shared_storage(&first, &second, &tree.binding_buffer_data());
}

fn assert_shared_storage(
    first: &[((u8, u8), BufferUpload)],
    second: &[((u8, u8), BufferUpload)],
    cleared: &[((u8, u8), BufferUpload)],
) {
    eprintln!(
        "one page: {} bytes; two bindings and two retained dispatch snapshots",
        bitmap(first, 0).len()
    );
    assert_eq!(
        bitmap(first, 0).as_ptr(),
        bitmap(first, 1).as_ptr(),
        "assignment duplicated the bitmap"
    );
    assert_eq!(
        bitmap(first, 0).as_ptr(),
        bitmap(second, 1).as_ptr(),
        "dispatch snapshot duplicated the bitmap"
    );
    assert_eq!(bitmap(cleared, 1), &[0; 4]);
    assert_eq!(bitmap(first, 1), bitmap(cleared, 0));
    assert!(bitmap(first, 1).len() > 2_000_000);
}

#[cfg(feature = "gpu")]
#[test]
fn repeated_page_draws_upload_each_immutable_bitmap_once_in_both_evaluators() {
    use easl::interpreter::{
        CaptureIO, CpuRuntime, IOManager, run_program_entry_with_io_and_runtime_from_path,
    };
    let source = format!(
        r#"{SOURCE}
@{{group 0 binding 2 address storage-write}} (var sampled: vec4f)
(var frame: u32)
@cpu (defn main []
  (setup)
  (spawn-window (fn []
    (swap)
    (dispatch-compute-shader (fn [] (= sampled (* 0.5 (+
      (texture-load bound (vec2u 1u) 0u) (texture-load page (vec2u 1u) 0u))))) (vec3u 1u))
    (print sampled)
    (+= frame 1u)
    (when (== frame 8u) (close-window)))))
"#
    );
    let program = program(&source);
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let (io, _) = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            CaptureIO::new(),
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("font-residency.easl"),
            runtime,
        )
        .unwrap();
        assert_eq!(io.prints.len(), 8);
        assert!(
            io.prints
                .iter()
                .all(|text| text.starts_with("(vec4f 1. 1. 1."))
        );
        let gpu = io.get_gpu().unwrap();
        let mut gpu = gpu.write().unwrap();
        let transfers = gpu.texture_transfer_stats();
        eprintln!("eight frames: {transfers:?}");
        assert_eq!(
            transfers.uploads, 1,
            "an unchanged font page was uploaded again"
        );
        assert_eq!(transfers.bytes, 2_932_736 * 2);
        assert!(transfers.read_only_reuses >= 8);
        assert_eq!(transfers.cached_read_only_pages, 1);
        assert_eq!(gpu.textures[&(0, 0)], gpu.textures[&(0, 1)]);
        // The completed program no longer owns the page. Neither the native
        // cache nor its old binding slots may keep the large GPU image alive.
        gpu.begin_frame();
        assert_eq!(gpu.texture_transfer_stats().cached_read_only_pages, 0);
        assert_eq!(gpu.textures[&(0, 0)].size().width, 1);
        assert_eq!(gpu.textures[&(0, 1)].size().width, 1);
        gpu.update_for_reload("@compute @workgroup_size(1) fn idle() {}", &[], &[]);
        assert!(gpu.textures.is_empty());
        assert!(gpu.texture_views.is_empty());
    }
}

#[cfg(feature = "gpu")]
#[test]
fn writable_replacement_detaches_from_shared_font_page_at_equal_dimensions() {
    use easl::interpreter::{
        CaptureIO, CpuRuntime, IOManager, run_program_entry_with_io_and_runtime_from_path,
    };
    let source = format!(
        r#"{SOURCE}
@{{group 0 binding 2 address storage-write}} (var sampled: vec4f)
@vertex (defn full []: @{{builtin position}} vec4f
  (vec4f (match (vertex-index) 0u (vec2f -1. -1.) 1u (vec2f 3. -1.) _ (vec2f -1. 3.)) 0. 1.))
@fragment (defn green []: @{{location 0}} vec4f (vec4f 0. 1. 0. 1.))
@cpu (defn main []
  (setup)
  (spawn-window (fn []
    (dispatch-compute-shader (fn [] (= sampled (+
      (texture-load bound (vec2u 1u) 0u) (texture-load page (vec2u 1u) 0u)))) (vec3u 1u))
    (print sampled)
    (= bound (blank-texture (texture-dimensions page)))
    (set-render-target bound) (dispatch-render-shaders full green 3u) (clear-render-target)
    (dispatch-compute-shader (fn [] (= sampled (texture-load bound (vec2u 1u) 0u))) (vec3u 1u))
    (print sampled)
    (dispatch-compute-shader (fn [] (= sampled (texture-load page (vec2u 1u) 0u))) (vec3u 1u))
    (print sampled)
    (close-window))))
"#
    );
    let program = program(&source);
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let (io, _) = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            CaptureIO::new(),
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("font-residency.easl"),
            runtime,
        )
        .unwrap();
        assert_eq!(io.prints.len(), 3);
        assert!(io.prints[0].starts_with("(vec4f 2. 2. 2."));
        assert_eq!(io.prints[1], "(vec4f 0. 1. 0. 1.)");
        assert!(io.prints[2].starts_with("(vec4f 1. 1. 1."));
        let gpu = io.get_gpu().unwrap();
        let gpu = gpu.read().unwrap();
        assert_ne!(gpu.textures[&(0, 0)], gpu.textures[&(0, 1)]);
        assert_eq!(gpu.textures[&(0, 0)].size(), gpu.textures[&(0, 1)].size());
        assert_eq!(gpu.texture_transfer_stats().uploads, 2);
    }
}

#[cfg(feature = "gpu")]
#[test]
fn equal_size_page_switches_keep_distinct_pixels_and_reuse_live_resources() {
    use easl::interpreter::{
        CaptureIO, CpuRuntime, IOManager, run_program_entry_with_io_and_runtime_from_path,
    };
    use std::sync::Arc;
    let source = r#"
@{group 0 binding 0} (var image: (Texture2D f32))
@{group 0 binding 1 address storage-write} (var sample: vec4f)
@cpu (defn main []
  (= image (blank-texture 2u 2u))
  (spawn-window (fn []
    (dispatch-compute-shader (fn [] (= sample (texture-load image (vec2u 0u) 0u))) (vec3u 1u))
    (print sample) (close-window))))
"#;
    let (io, _) = run_program_entry_with_io_and_runtime_from_path(
        program(source),
        Some("main"),
        CaptureIO::new(),
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("font-residency.easl"),
        CpuRuntime::BytecodeVm,
    )
    .unwrap();
    let handle = io.get_gpu().unwrap();
    let mut gpu = handle.write().unwrap();
    let mut red: Arc<[u8]> = [255, 0, 0, 255].repeat(4).into();
    let green: Arc<[u8]> = [0, 255, 0, 255].repeat(4).into();
    let upload = |data: &Arc<[u8]>, width, height| {
        [(
            (0, 0),
            BufferUpload::TextureData {
                width,
                height,
                data: data.clone(),
                read_only: true,
            },
        )]
    };
    let baseline = gpu.texture_transfer_stats();
    gpu.upload_bindings(&upload(&red, 2, 2));
    let original = gpu.textures[&(0, 0)].clone();
    assert_eq!(gpu.read_texture(0, 0).unwrap().2.as_slice(), red.as_ref());
    gpu.upload_bindings(&upload(&green, 2, 2));
    assert_ne!(original, gpu.textures[&(0, 0)]);
    assert_eq!(gpu.read_texture(0, 0).unwrap().2.as_slice(), green.as_ref());
    gpu.upload_bindings(&upload(&red, 2, 2));
    assert_eq!(original, gpu.textures[&(0, 0)]);
    assert_eq!(gpu.read_texture(0, 0).unwrap().2.as_slice(), red.as_ref());
    assert_eq!(gpu.texture_transfer_stats().uploads - baseline.uploads, 2);
    // Equal byte length with another shape must not alias the old dimensions.
    gpu.upload_bindings(&upload(&red, 1, 4));
    let (width, height, pixels) = gpu.read_texture(0, 0).unwrap();
    assert_eq!((width, height), (1, 4));
    assert_eq!(pixels.as_slice(), red.as_ref());
    assert_ne!(original, gpu.textures[&(0, 0)]);
    gpu.upload_bindings(&upload(&red, 2, 2));
    assert_eq!(original, gpu.textures[&(0, 0)]);
    assert_eq!(gpu.texture_transfer_stats().uploads - baseline.uploads, 3);
    assert_eq!(gpu.texture_transfer_stats().cached_read_only_pages, 3);
    drop(green);
    gpu.begin_frame();
    assert_eq!(gpu.texture_transfer_stats().cached_read_only_pages, 2);
    // An embedding host can explicitly mutate its own pixels through copy on
    // write. Weak cache ownership must force a fresh identity and fresh upload.
    Arc::make_mut(&mut red)[..4].copy_from_slice(&[0, 0, 255, 255]);
    gpu.upload_bindings(&upload(&red, 2, 2));
    assert_eq!(gpu.read_texture(0, 0).unwrap().2.as_slice(), red.as_ref());
    assert_ne!(original, gpu.textures[&(0, 0)]);
    assert_eq!(gpu.texture_transfer_stats().cached_read_only_pages, 1);
    assert_eq!(gpu.texture_transfer_stats().uploads - baseline.uploads, 4);
    drop(red);
    gpu.begin_frame();
    assert_eq!(gpu.texture_transfer_stats().cached_read_only_pages, 0);
    assert_eq!(gpu.textures[&(0, 0)].size().width, 1);
}
