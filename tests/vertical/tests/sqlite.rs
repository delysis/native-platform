use rusqlite::Connection;
use sha2::{Digest, Sha256};

#[test]
fn root_sqlite_identity_and_compile_options_are_stable() {
    let connection = Connection::open_in_memory().expect("open bundled SQLite");
    let version: String = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .expect("read SQLite version");
    let mut statement = connection
        .prepare("SELECT compile_options FROM pragma_compile_options ORDER BY compile_options")
        .expect("prepare compile-options query");
    let options = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query compile options")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect compile options");
    let serialized = options.join("\n");
    let digest = format!("{:x}", Sha256::digest(serialized.as_bytes()));

    assert_eq!(rusqlite::version(), version);
    assert_eq!(version, "3.51.3");
    assert!(options.iter().any(|option| option == "THREADSAFE=1"));
    assert!(options.iter().any(|option| option == "ENABLE_FTS5"));
    assert_eq!(
        digest, "237dbc028deb283af23c96fd82473d36055b11b437c9b282be65e50b1a2acd36",
        "bundled SQLite compile options changed:\n{serialized}"
    );
}
