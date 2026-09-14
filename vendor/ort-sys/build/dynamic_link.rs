#[cfg(feature = "copy-dylibs")]
use std::path::Path;

use crate::vars;

pub fn prefer_dynamic_linking() -> bool {
	match vars::get(vars::PREFER_DYNAMIC_LINK) {
		Some(val) => val == "1" || val.to_lowercase() == "true",
		None => false
	}
}

#[cfg(feature = "copy-dylibs")]
pub fn copy_dylibs(lib_dir: &Path, out_dir: &Path) {
	use std::fs;

	// get the target directory - we need to place the dlls next to the executable so they can be properly loaded by windows
	let out_dir = out_dir.ancestors().nth(3).unwrap();
	for out_dir in [out_dir.to_path_buf(), out_dir.join("examples"), out_dir.join("deps")] {
		fs::create_dir_all(&out_dir).unwrap();
		let lib_files = fs::read_dir(lib_dir).unwrap_or_else(|_| panic!("Failed to read contents of `{}` (does it exist?)", lib_dir.display()));
		for lib_file in lib_files.filter(|e| {
			e.as_ref().ok().is_some_and(|e| {
				e.file_type().is_ok_and(|e| !e.is_dir()) && [".dll", ".so", ".dylib"].into_iter().any(|v| e.path().to_string_lossy().contains(v))
			})
		}) {
			let lib_file = lib_file.unwrap();
			let lib_path = lib_file.path();
			let lib_name = lib_path.file_name().unwrap();
			let out_path = out_dir.join(lib_name);
			if out_path.is_symlink() {
				fs::remove_file(&out_path).unwrap();
			}
			// Windows build caches must contain the DLL bytes, not links into
			// an external download cache that may be absent on the next runner.
			#[cfg(windows)]
			fs::copy(&lib_path, &out_path).unwrap();
			#[cfg(unix)]
			if !out_path.exists() {
				std::os::unix::fs::symlink(&lib_path, &out_path).unwrap();
			}
			println!("cargo:rerun-if-changed={}", out_path.to_str().unwrap());
		}

		// Windows searches the executable directory before system directories
		// and PATH, so every executable directory needs its matching DLLs.
	}
}
