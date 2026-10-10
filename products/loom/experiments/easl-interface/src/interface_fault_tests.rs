use super::*;

fn program(body: &str) -> String {
    format!(
        "@external (var input: [{INPUT_COUNT}: f32] (zeroed-array)) \
         @cpu (defn main [] {body} (print \"state\") (print (vec4f 0. 0. 0. 19.)))"
    )
}

#[test]
fn nonfinite_host_input_does_not_poison_an_unexecuted_vm() {
    let mut ui = Interface::compile(&program("")).unwrap();
    let mut input = [0.; INPUT_COUNT];
    input[3] = f32::NAN;
    assert!(ui.step(input).unwrap_err().contains("Nonfinite"));
    input[3] = 0.;
    assert!(ui.step(input).is_ok());
}

#[test]
fn malformed_display_list_faults_until_explicit_replacement() {
    let source = program(
        "(when (== (input 2u) 1.) (print \"unknown-command\") (print (vec4f 0. 0. 0. 0.)))",
    );
    let mut ui = Interface::compile(&source).unwrap();
    let mut input = [0.; INPUT_COUNT];
    assert!(ui.step(input).is_ok());
    input[2] = 1.;
    assert!(ui.step(input).is_err());
    input[2] = 0.;
    assert!(ui.step(input).unwrap_err().contains("faulted"));
    assert!(Interface::compile(&source).unwrap().step(input).is_ok());
}

#[test]
fn exhausted_execution_budget_cannot_resume_a_partially_executed_vm() {
    let source = format!(
        "@external (var input: [{INPUT_COUNT}: f32] (zeroed-array)) \
         @cpu (defn main [] (while true ()))"
    );
    let mut ui = Interface::compile(&source).unwrap();
    let mut input = [0.; INPUT_COUNT];
    input[2] = 1.;
    assert!(ui.step(input).is_err());
    input[2] = 0.;
    assert!(ui.step(input).unwrap_err().contains("faulted"));
}

#[test]
fn invalid_output_does_not_return_actions_emitted_before_the_error() {
    let source = program(
        "(print \"action\") (print (vec4f 6. 0. 0. 0.)) \
         (print \"unknown-command\") (print (vec4f 0. 0. 0. 0.))",
    );
    let mut ui = Interface::compile(&source).unwrap();
    assert!(ui.step([0.; INPUT_COUNT]).is_err());
    assert!(ui.step([0.; INPUT_COUNT]).unwrap_err().contains("faulted"));
}
