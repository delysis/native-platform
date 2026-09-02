ALTER TABLE documents ADD COLUMN display_title TEXT
    CHECK (
        display_title IS NULL OR (
            length(CAST(display_title AS BLOB)) BETWEEN 1 AND 256
            -- Rust's `str::trim` follows Unicode White_Space, while SQLite's
            -- one-argument trim only recognizes U+0020. Keep the database
            -- canonical representation identical to the Rust command path.
            AND unicode(substr(display_title, 1, 1)) NOT IN (
                9, 10, 11, 12, 13, 32, 133, 160, 5760,
                8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
                8232, 8233, 8239, 8287, 12288
            )
            AND unicode(substr(display_title, -1, 1)) NOT IN (
                9, 10, 11, 12, 13, 32, 133, 160, 5760,
                8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
                8232, 8233, 8239, 8287, 12288
            )
            -- `char::is_control` is Unicode General_Category=Cc: U+0000-001F
            -- and U+007F-009F. NUL needs its own predicate because it cannot
            -- participate in a SQLite GLOB pattern.
            AND instr(display_title, char(0)) = 0
            AND display_title NOT GLOB ('*[' || char(1) || '-' || char(31) || ']*')
            AND display_title NOT GLOB ('*[' || char(127) || '-' || char(159) || ']*')
        )
    );
