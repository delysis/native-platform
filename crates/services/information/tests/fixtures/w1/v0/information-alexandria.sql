PRAGMA page_size = 4096;
PRAGMA auto_vacuum = NONE;
PRAGMA application_id = 1229868107;
PRAGMA user_version = 1;
CREATE TABLE documents (
    doc_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    author_normalized TEXT,
    author_attributed TEXT,
    tradition_tag TEXT NOT NULL,
    date_range TEXT,
    language_original TEXT,
    language_translation TEXT,
    translator TEXT,
    editor TEXT,
    edition TEXT,
    source_uri TEXT NOT NULL,
    rights_status TEXT NOT NULL,
    genre TEXT,
    file_ext TEXT NOT NULL,
    ingest_status TEXT NOT NULL,
    block_count INTEGER NOT NULL,
    text_chars INTEGER NOT NULL,
    canonical_path TEXT
);
CREATE TABLE blocks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT UNIQUE NOT NULL,
    doc_id TEXT NOT NULL REFERENCES documents(doc_id),
    block_index INTEGER NOT NULL,
    block_type TEXT NOT NULL,
    text TEXT NOT NULL,
    char_start INTEGER,
    char_end INTEGER,
    location_path TEXT
);
CREATE INDEX idx_blocks_doc_idx ON blocks(doc_id, block_index);
CREATE VIRTUAL TABLE blocks_fts USING fts5(
    block_id UNINDEXED,
    doc_id UNINDEXED,
    text,
    tokenize = 'unicode61'
);
CREATE TABLE block_theme_hits (
    hit_id INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id TEXT NOT NULL,
    block_id TEXT NOT NULL,
    theme_tag TEXT NOT NULL,
    matched_term TEXT NOT NULL,
    controversy_risk TEXT NOT NULL
);
CREATE INDEX idx_theme_hits_block ON block_theme_hits(block_id);
INSERT INTO documents (
    doc_id,
    title,
    author_normalized,
    tradition_tag,
    language_translation,
    source_uri,
    rights_status,
    genre,
    file_ext,
    ingest_status,
    block_count,
    text_chars,
    canonical_path
) VALUES (
    'W1:DOC:0001',
    'The Fixture Treatise',
    'A. Mystic',
    'Christian contemplative',
    'English',
    'fixture://w1/treatise',
    'public_domain',
    'spirituality',
    'txt',
    'ok',
    1,
    58,
    'fixture/treatise.txt'
);
INSERT INTO blocks (
    block_id,
    doc_id,
    block_index,
    block_type,
    text,
    location_path
) VALUES (
    'W1:DOC:0001:B000001',
    'W1:DOC:0001',
    1,
    'paragraph',
    'The prayer of quiet gathers the powers for contemplation.',
    'chapter 1'
);
INSERT INTO blocks_fts (block_id, doc_id, text)
SELECT block_id, doc_id, text FROM blocks;
VACUUM;
