use crate::Error;
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    interpreter::{
        BufferUpload, EvalError, FrameDriver, IOManager, UserspaceEvalError, VmCpuRuntime,
        WindowEvent,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::{path::PathBuf, sync::Arc};

pub(crate) struct Runtime {
    pub vm: VmCpuRuntime<BudgetIo>,
    pub external: Arc<ExternalVars>,
}

impl Runtime {
    pub fn compile(sources: &[(&str, &str)]) -> Result<Self, Error> {
        let mut documents = EaslMultiDocument {
            sources: Vec::new(),
        };
        for &(name, source) in sources {
            let parsed = parse_easl_without_comments(source);
            if !parsed.parsing_failures.is_empty() {
                return Err(Error::Language(format!(
                    "Parse error in {name}: {:?}",
                    parsed.parsing_failures
                )));
            }
            documents.add_document(parsed, name.into(), source.into());
        }
        Self::from_documents(documents)
    }
    pub fn from_documents(documents: EaslMultiDocument) -> Result<Self, Error> {
        let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
        if !errors.is_empty() {
            return Err(Error::Language(format!("{errors:?}")));
        }
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        if !errors.is_empty() {
            return Err(Error::Language(format!("{errors:?}")));
        }
        let external = ExternalVars::new(&program);
        let vm = VmCpuRuntime::new_cpu_with_external(
            program,
            BudgetIo { remaining: 500_000 },
            None::<PathBuf>,
            Some(external.clone()),
        )
        .map_err(|error| Error::Language(error.to_string()))?;
        Ok(Self { vm, external })
    }
    pub fn run(&mut self, entry: &str) -> Result<(), Error> {
        self.vm.env.io.remaining = 500_000;
        self.vm
            .run(entry)
            .map_err(|error| Error::Language(error.to_string()))?;
        Ok(())
    }
    pub fn write(&self, name: &str, words: &[u32]) -> Result<(), Error> {
        self.external
            .write_external_var_raw(name, words)
            .map_err(|error| Error::Language(error.to_string()))
    }
    pub fn read(&self, name: &str) -> Result<Vec<u32>, Error> {
        self.external
            .read_external_var_raw(name)
            .map_err(|error| Error::Language(error.to_string()))
    }
}

pub(crate) struct BudgetIo {
    remaining: u32,
}
fn denied() -> EvalError {
    UserspaceEvalError::RuntimeError("Text composition has no device I/O".into()).into()
}
impl IOManager for BudgetIo {
    fn check_execution(&mut self) -> Result<(), EvalError> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            EvalError::from(UserspaceEvalError::RuntimeError(
                "Text VM work limit".into(),
            ))
        })?;
        Ok(())
    }
    fn println(&mut self, _: &str) {
        self.remaining = 0;
    }
    fn record_draw(
        &mut self,
        _: u16,
        _: u16,
        _: &str,
        _: &str,
        _: u32,
        _: Vec<((u8, u8), BufferUpload)>,
        _: easl::interpreter::RenderBlend,
        _: Option<(u8, u8)>,
    ) -> Result<(), EvalError> {
        Err(denied())
    }
    fn record_compute(
        &mut self,
        _: u16,
        _: &str,
        _: (u32, u32, u32),
        _: Vec<((u8, u8), BufferUpload)>,
    ) -> Result<(), EvalError> {
        Err(denied())
    }
    fn take_frame_draw_calls(&mut self) -> Vec<WindowEvent> {
        Vec::new()
    }
    fn record_close_window(&mut self) {
        self.remaining = 0;
    }
    fn sync_gpu_to_cpu(&mut self, _: u8, _: u8, _: u64) -> Option<Vec<u8>> {
        None
    }
    fn run_spawn_window_driver<D: FrameDriver<IO = Self>>(_: &mut D) -> Result<bool, EvalError> {
        Err(denied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_cpu_calls_publish_scalars_and_buffers_and_adopt_new_inputs() {
        let source = "@external (var seed: u32)\n@external (var result: u32)\n@external (var values: [u32])\n@cpu (defn main [] (= values (zeroed-array 2u)) (= (values 0u) seed) (= (values 1u) (+ seed 1u)) (= result (+ seed 2u)))";
        let mut runtime = Runtime::compile(&[("cpu-exchange.easl", source)]).unwrap();
        for seed in [7, 23] {
            runtime.write("seed", &[seed]).unwrap();
            runtime.run("main").unwrap();
            assert_eq!(runtime.read("result").unwrap(), vec![seed + 2]);
            assert_eq!(runtime.read("values").unwrap(), vec![seed, seed + 1]);
            assert_eq!(runtime.read("seed").unwrap(), vec![seed]);
        }
    }

    #[test]
    fn local_copies_do_not_alias_initializers_or_surrounding_loop_counters() {
        let source = "@external (var result: vec4u)\n@cpu (defn main [] (let [@var original (vec2u 3u 4u) saved original.x @var copied original @var iterations 0u] (= original.x 8u) (= copied.y 99u) (for [end 1u (< end 4u) (+= end 1u)] (let [@var start end] (while (> start 0u) (-= start 1u) (+= iterations 1u)))) (= result (vec4u saved original.y copied.x iterations))))";
        let mut runtime = Runtime::compile(&[("value-ownership.easl", source)]).unwrap();
        runtime.run("main").unwrap();
        assert_eq!(runtime.read("result").unwrap(), vec![3, 4, 3, 6]);
    }

    #[test]
    fn an_interrupted_cpu_entry_does_not_publish_its_partial_output() {
        let source =
            "@external (var result: u32) @cpu (defn main [] (= result 99u) (while true ()))";
        let mut runtime = Runtime::compile(&[("interrupted.easl", source)]).unwrap();
        runtime.write("result", &[7]).unwrap();
        runtime.vm.env.io.remaining = 1;
        assert!(runtime.vm.run("main").is_err());
        assert_eq!(runtime.read("result").unwrap(), vec![7]);
    }
}
