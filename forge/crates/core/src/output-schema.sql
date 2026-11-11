CREATE TABLE output_chunks(
    run_id TEXT NOT NULL REFERENCES runs(id),
    role TEXT NOT NULL CHECK(role IN ('assistant','tool')),
    offset INTEGER NOT NULL CHECK(offset>=0),
    body TEXT NOT NULL,
    byte_length INTEGER NOT NULL CHECK(byte_length>0 AND byte_length<=65536),
    PRIMARY KEY(run_id,role,offset)
);
