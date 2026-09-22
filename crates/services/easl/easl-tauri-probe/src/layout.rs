//! Fixed, embedded EASL layout: no application policy and no file/device I/O.
use crate::{Error, Rect};
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

const SOURCE: &str = include_str!("../ui/two_fields.easl");

pub(crate) struct Layout {
    runtime: VmCpuRuntime<Budget>,
    external: Arc<ExternalVars>,
    faulted: bool,
}
impl Layout {
    pub fn new() -> Result<Self, Error> {
        std::panic::catch_unwind(|| {
            let parsed = parse_easl_without_comments(SOURCE);
            if !parsed.parsing_failures.is_empty() {
                return Err(Error::Layout);
            }
            let documents = EaslMultiDocument::from_singular_document(
                parsed,
                "two_fields.easl".into(),
                SOURCE.into(),
            );
            let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
            if !errors.is_empty()
                || !program
                    .validate_raw_program(CompilerTarget::WGSL)
                    .is_empty()
            {
                return Err(Error::Layout);
            }
            let external = ExternalVars::new(&program);
            let runtime = VmCpuRuntime::new_cpu_with_external(
                program,
                Budget(4096),
                None::<PathBuf>,
                Some(external.clone()),
            )
            .map_err(|_| Error::Layout)?;
            Ok(Self {
                runtime,
                external,
                faulted: false,
            })
        })
        .map_err(|_| Error::Layout)?
    }
    pub fn boxes(&mut self, size: [f32; 2]) -> Result<[Rect; 2], Error> {
        // Bad host geometry never enters or poisons the interpreter.
        if !size.iter().all(|v| v.is_finite())
            || size[0] < 64.
            || size[1] < 96.
            || size.iter().any(|v| *v > 16_384.)
        {
            return Err(Error::Geometry);
        }
        if self.faulted {
            return Err(Error::Layout);
        }
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(size)))
        {
            Ok(result) => result,
            Err(_) => Err(Error::Layout),
        };
        if result.is_err() {
            self.faulted = true;
        }
        result
    }
    fn run(&mut self, size: [f32; 2]) -> Result<[Rect; 2], Error> {
        self.external
            .write_external_var_raw("viewport", &size.map(f32::to_bits))
            .map_err(|_| Error::Layout)?;
        self.runtime.env.io.0 = 4096;
        self.runtime.run("main").map_err(|_| Error::Layout)?;
        let mut boxes = [Rect([0.; 4]); 2];
        for (rect, name) in boxes.iter_mut().zip(["field-one", "field-two"]) {
            let words = self
                .external
                .read_external_var_raw(name)
                .map_err(|_| Error::Layout)?;
            let words: [u32; 4] = words.try_into().map_err(|_| Error::Layout)?;
            *rect = Rect(words.map(f32::from_bits));
            rect.validate_inside(size)?;
        }
        if boxes[0].0[1] + boxes[0].0[3] > boxes[1].0[1] {
            return Err(Error::Geometry);
        }
        Ok(boxes)
    }
}

struct Budget(u32);
fn denied() -> EvalError {
    UserspaceEvalError::RuntimeError("Native surface layout has no device I/O".into()).into()
}
impl IOManager for Budget {
    fn check_execution(&mut self) -> Result<(), EvalError> {
        self.0 = self.0.checked_sub(1).ok_or_else(denied)?;
        Ok(())
    }
    fn println(&mut self, _: &str) {
        self.0 = 0;
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
        self.0 = 0;
    }
    fn sync_gpu_to_cpu(&mut self, _: u8, _: u8, _: u64) -> Option<Vec<u8>> {
        None
    }
    fn run_spawn_window_driver<D: FrameDriver<IO = Self>>(_: &mut D) -> Result<bool, EvalError> {
        Err(denied())
    }
}
