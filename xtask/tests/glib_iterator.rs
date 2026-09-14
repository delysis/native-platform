//! Exercise the GLib C output pointer fixed by RUSTSEC-2024-0429.
//! CI also runs this target with optimization, where the original aliasing bug
//! could let the compiler treat that pointer as permanently null.
#![cfg(target_os = "linux")]

use glib::prelude::*;

#[test]
fn string_iterator_reads_native_output_in_both_directions() {
    let values = ["first", "λ", "", "last"].to_variant();
    let mut iter = values.array_iter_str().expect("string array");
    assert_eq!(iter.next(), Some("first"));
    assert_eq!(iter.next_back(), Some("last"));
    assert_eq!(iter.next(), Some("λ"));
    assert_eq!(iter.next_back(), Some(""));
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
}
