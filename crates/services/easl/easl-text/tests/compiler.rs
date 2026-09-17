//! Preserve shader inputs while avoiding full effect graphs during extraction.
use easl::{
    CompilerTarget,
    compiler::{
        builtins::built_in_macros, entry::BuiltinIOAttribute,
        functions::FunctionImplementationKind, program::Program, types::Type,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::{collections::HashSet, path::Path};

fn parse(source: &str) -> Program {
    let documents =
        EaslMultiDocument::from_singular_document_sourceless(parse_easl_without_comments(source));
    let (program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    program
}

#[test]
fn extracted_shader_inputs_match_full_effect_analysis_through_shared_helpers() {
    let mut sources = vec![String::from(
        "(defn leaf []: u32 (vertex-index))
         (defn left []: u32 (+ (leaf) (leaf)))
         (defn right []: u32 (+ (left) (leaf)))
         @vertex (defn vertex []: @{builtin position} vec4f
           (vec4f (f32 (+ (left) (right))) 0. 0. 1.))
         @cpu (defn main [] (print 3u))",
    )];
    for name in [
        "get_global_invocation_id",
        "get_global_invocation_id_argument",
        "get_global_invocation_id_argument_field",
        "higher_order_sdf_ops_closures",
    ] {
        sources.push(
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("../vendor/easl/data/gpu/{name}.easl")),
            )
            .unwrap(),
        );
    }
    for source in sources {
        let mut program = parse(&source);
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(errors.is_empty(), "{errors:?}");
        let mut expected = HashSet::new();
        for function in program.abstract_functions_iter() {
            let function = function.read().unwrap();
            if let FunctionImplementationKind::Composite(implementation) = &function.implementation
            {
                let implementation = implementation.read().unwrap();
                // Independent full analysis supplies the oracle, including
                // transitive helpers, closures and explicit argument fields.
                let attributes = implementation.effects().looked_up_builtin_attributes();
                expected.extend(attributes.iter().copied());
                if function.entry_point.is_some() {
                    let Type::Function(signature) = implementation.expression.data.unwrap_known()
                    else {
                        panic!("entry must be a function")
                    };
                    for attribute in attributes {
                        assert!(
                            implementation
                                .arg_annotations
                                .iter()
                                .any(|arg| { arg.attributes.has_builtin_io_attribute(attribute) })
                                || signature.args.iter().any(|(arg, _)| {
                                    if let Type::Struct(record) = arg.var_type.unwrap_known() {
                                        record.fields.iter().any(|field| {
                                            field.attributes.has_builtin_io_attribute(attribute)
                                        })
                                    } else {
                                        false
                                    }
                                }),
                            "{} lacks {attribute:?}",
                            function.name,
                        );
                    }
                }
            }
        }
        let actual = BuiltinIOAttribute::all()
            .into_iter()
            .filter(|attribute| {
                program
                    .top_level_vars
                    .iter()
                    .any(|var| var.name.as_ref() == attribute.compiled_name())
            })
            .collect::<HashSet<_>>();
        assert!(!expected.is_empty());
        assert_eq!(actual, expected);
    }
}

#[test]
fn transitive_builtin_lookup_still_rejects_the_wrong_shader_stage() {
    let mut program = parse(
        "(defn leaf []: u32 (.x (global-invocation-id)))
         (defn shared []: u32 (+ (leaf) (leaf)))
         @fragment (defn fragment []: @{location 0} vec4f
           (vec4f (f32 (shared))))",
    );
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(
        format!("{errors:?}").contains("InvalidBuiltinForEntryPoint"),
        "{errors:?}"
    );
}

#[test]
fn shared_effect_graph_preserves_transitive_mutation_and_refreshes_after_rewrites() {
    let mut source = String::from(
        "(var before: f32) (var after: f32) (var sink: f32)
         (defn leaf []: f32 (= sink before) before)
         (defn replacement []: f32 (= sink after) after)\n",
    );
    let mut previous = String::from("leaf");
    for level in 0..18 {
        let name = format!("shared-{level}");
        source += &format!("(defn {name} []: f32 (+ ({previous}) ({previous})))\n");
        previous = name;
    }
    source += &format!("@cpu (defn main [] (print ({previous})))");
    let mut program = parse(&source);
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let implementation = |name: &str| {
        program
            .abstract_functions_iter()
            .find_map(|function| {
                let function = function.read().unwrap();
                if function.name.as_ref() == name
                    && let FunctionImplementationKind::Composite(implementation) =
                        &function.implementation
                {
                    Some(implementation.clone())
                } else {
                    None
                }
            })
            .unwrap()
    };
    let root = implementation(&previous);
    let (reads, writes) = root.read().unwrap().effects().read_and_written_globals();
    assert!(reads.iter().any(|name| name.as_ref() == "before"));
    assert!(!reads.iter().any(|name| name.as_ref() == "after"));
    assert!(writes.iter().any(|name| name.as_ref() == "sink"));
    let replacement = implementation("replacement")
        .read()
        .unwrap()
        .expression
        .clone();
    implementation("leaf").write().unwrap().expression = replacement;
    let (reads, writes) = root.read().unwrap().effects().read_and_written_globals();
    assert!(!reads.iter().any(|name| name.as_ref() == "before"));
    assert!(reads.iter().any(|name| name.as_ref() == "after"));
    assert!(writes.iter().any(|name| name.as_ref() == "sink"));
}

#[test]
fn flat_font_indices_emit_one_valid_interpolation_attribute() {
    let mut program = parse(
        "(struct GlyphVertex
           @{builtin position} clip: vec4f
           @{location 0 interpolate flat} font: u32)
         @vertex (defn glyph-vertex []: GlyphVertex (GlyphVertex (vec4f 0. 0. 0. 1.) 2u))
         @fragment (defn glyph-fragment [v: GlyphVertex]: @{location 0} vec4f
           (vec4f (f32 v.font)))",
    );
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let shader = program.compile_to_target(CompilerTarget::WGSL).unwrap();
    assert!(shader.contains("@interpolate(flat, first)"), "{shader}");
    assert!(!shader.contains("@interpolate(@"), "{shader}");
}

#[test]
fn interpolation_modes_emit_valid_attributes() {
    for (mode, expected) in [
        ("perspective", "perspective, center"),
        ("perspective-centroid", "perspective, centroid"),
        ("perspective-sample", "perspective, sample"),
        ("linear", "linear, center"),
        ("linear-centroid", "linear, centroid"),
        ("linear-sample", "linear, sample"),
        ("flat", "flat, first"),
        ("flat-first", "flat, first"),
        ("flat-either", "flat, either"),
    ] {
        let mut program = parse(&format!(
            "(struct Vertex
               @{{builtin position}} clip: vec4f
               @{{location 0 interpolate {mode}}} value: f32)
             @vertex (defn vertex []: Vertex (Vertex (vec4f 0. 0. 0. 1.) 2.))
             @fragment (defn fragment [v: Vertex]: @{{location 0}} vec4f (vec4f v.value))"
        ));
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(errors.is_empty(), "{mode}: {errors:?}");
        let shader = program.compile_to_target(CompilerTarget::WGSL).unwrap();
        assert!(
            shader.contains(&format!("@interpolate({expected})")),
            "{shader}"
        );
        assert!(!shader.contains("@interpolate(@"), "{shader}");
    }
}

#[test]
fn malformed_interpolation_returns_diagnostics_without_slicing_utf8() {
    for mode in [
        "x",
        "é",
        "perspectiveé",
        "flat-",
        "flat-center",
        "linear-first",
    ] {
        let source = format!(
            "(struct Vertex
               @{{builtin position}} clip: vec4f
               @{{location 0 interpolate {mode}}} value: f32)
             @vertex (defn vertex []: Vertex (Vertex (vec4f 0. 0. 0. 1.) 2.))"
        );
        let documents = EaslMultiDocument::from_singular_document_sourceless(
            parse_easl_without_comments(&source),
        );
        let (_, errors) = Program::from_easl_documents(&documents, built_in_macros());
        assert!(
            format!("{errors:?}").contains("InvalidInterpolation"),
            "{mode}: {errors:?}"
        );
    }
}
