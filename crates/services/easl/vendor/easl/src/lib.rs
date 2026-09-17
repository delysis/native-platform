#[cfg(feature = "window")]
pub mod audio;
pub mod compiler;
pub mod external;
pub mod font;
pub mod format;
pub mod input;
pub mod interpreter;
pub mod parse;
pub mod text;
pub mod thread_sync;
pub mod vm;
#[cfg(feature = "window")]
pub mod window;

#[derive(Debug)]
pub(crate) enum Never {}

pub use compiler::core::compile_easl_file_to_target;
pub use compiler::core::compile_easl_file_to_wgsl;
pub use compiler::core::compile_easl_source_to_target;
pub use compiler::core::compile_easl_source_to_wgsl;
pub use compiler::core::get_easl_program_info;
pub use compiler::core::load_easl_program_from_file;
pub use compiler::program::CompilerTarget;
pub use format::format_easl_source;
