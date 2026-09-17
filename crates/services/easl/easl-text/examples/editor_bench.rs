//! Measures actual EASL editing/history against retained native text values.
//! Excludes shaping, painting, platform event delivery and application services.
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    interpreter::{StringIO, VmCpuRuntime},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::{error::Error, path::PathBuf, time::Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let source = r#"
      (var bench-state: TextEditorState)
      (var bench-insert: TextBuffer)
      @external (var bench-input: [u32])
      @external (var bench-length: u32)
      @cpu (defn bench-setup []
        (let [seed (text-from-utf8 bench-input 0u (array-length bench-input))]
          (text-editor-open bench-state seed) (text-release seed))
        (= bench-insert (make-text "x"))
        (text-editor-edit bench-state (TextEditCommand 3u (text-length bench-state.current.text) 0u 0u) (TextBuffer 0u)) ())
      @cpu (defn bench-append []
        (text-editor-commit bench-state bench-insert)
        (= bench-length (text-length bench-state.current.text)))
      @cpu (defn bench-navigate []
        (text-editor-navigate bench-state 3u false false false)
        (text-editor-navigate bench-state 4u false false false) ())
      @cpu (defn bench-close []
        (text-editor-close bench-state) (text-release bench-insert))
    "#;
    let mut docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "editor-bench.easl".into(),
        source.into(),
    );
    for (name, source) in [
        ("editor.easl", include_str!("../library/editor.easl")),
        ("input.easl", include_str!("../library/input.easl")),
        ("editing.easl", include_str!("../library/editing.easl")),
    ] {
        docs.add_document(
            parse_easl_without_comments(source),
            name.into(),
            source.into(),
        );
    }
    let start = Instant::now();
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
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
        None::<PathBuf>,
        Some(external.clone()),
    )?;
    println!(
        "editor_compile_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.
    );
    for bytes in [1024, 16_384, 131_072, 1_047_552] {
        external.write_external_var_raw("bench-input", &vec![u32::from(b'a'); bytes])?;
        vm.run("bench-setup")?;
        for entry in ["bench-append", "bench-navigate"] {
            let mut timings = Vec::new();
            for i in 0..51 {
                let start = Instant::now();
                vm.run(entry)?;
                if i != 0 {
                    timings.push(start.elapsed().as_secs_f64() * 1000.);
                }
            }
            timings.sort_by(f64::total_cmp);
            println!(
                "bytes={bytes} operation={entry} p50_ms={:.3} p95_ms={:.3} retained_bytes={} samples=50",
                timings[24],
                timings[47],
                vm.env.text_values.retained_bytes()
            );
        }
        assert_eq!(
            external.read_external_var_raw("bench-length")?,
            vec![bytes as u32 + 51]
        );
        vm.run("bench-close")?;
        assert_eq!(vm.env.text_values.live_values(), 0);
    }
    Ok(())
}
