//! Pre-generated fixture content for sandbox test environments.
//!
//! These fixtures are written into each sandbox before command execution,
//! providing text patterns useful for exercising various diff/format options.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Directory name for fixtures in the sandbox.
const FIXTURES_DIR: &str = "_fixtures";

/// Repeated similar blocks - tests diff algorithm options
/// (--patience, --minimal, --histogram, --diff-algorithm).
const REPEATED: &str = r#"section_start {
    process_item alpha
    validate alpha
    save alpha
}
section_end

section_start {
    process_item beta
    validate beta
    save beta
}
section_end

section_start {
    process_item gamma
    validate gamma
    save gamma
}
section_end

section_start {
    process_item delta
    validate delta
    save delta
}
section_end

section_start {
    process_item epsilon
    validate epsilon
    save epsilon
}
section_end

handler_block {
    setup_connection
    read_data
    process_data
    write_result
    cleanup
}

handler_block {
    setup_connection
    read_data
    process_data
    write_result
    cleanup
}

handler_block {
    setup_connection
    read_data
    process_data
    write_result
    cleanup
}
"#;

/// Indented code-like structure - tests indent heuristics
/// (--indent-heuristic, --no-indent-heuristic).
const INDENTED: &str = r#"def function_one():
    setup()
    process()
    cleanup()
    return True

def function_two():
    setup()
    process()
    cleanup()
    return True

def function_three():
    setup()
    process()
    cleanup()
    return True

class Handler:
    def __init__(self):
        self.state = None

    def handle(self, data):
        self.validate(data)
        self.transform(data)
        self.store(data)

    def validate(self, data):
        pass

    def transform(self, data):
        pass

    def store(self, data):
        pass

class Processor:
    def __init__(self):
        self.state = None

    def handle(self, data):
        self.validate(data)
        self.transform(data)
        self.store(data)

    def validate(self, data):
        pass

    def transform(self, data):
        pass

    def store(self, data):
        pass
"#;

/// Moveable blocks - tests copy/move detection
/// (-C, -M, --color-moved, --no-color-moved-ws).
const MOVEABLE: &str = r#"# Configuration File
# This content can be reordered to test move detection

[database]
host = localhost
port = 5432
name = myapp_db
user = admin

[cache]
host = localhost
port = 6379
ttl = 3600

[logging]
level = info
format = json
output = stdout

[server]
host = 0.0.0.0
port = 8080
workers = 4

[features]
enable_auth = true
enable_cache = true
enable_logging = true

# End of configuration
"#;

/// Whitespace variations - tests whitespace ignore options
/// (--ignore-all-space, --ignore-space-change, --ignore-space-at-eol).
const WHITESPACE: &str = "line with trailing spaces   \n\
line\twith\ttabs\n\
    four space indent\n\
\tsingle tab indent\n\
  \t  mixed spaces and tab  \n\
normal line no trailing\n\
  leading spaces only\n\
\t\tdouble tab indent\n";

/// CRLF line endings - tests CR handling (--ignore-cr-at-eol).
const CRLF: &str = "line one with crlf\r\n\
line two with crlf\r\n\
line three with crlf\r\n\
line four with crlf\r\n";

/// C functions - tests function context (--function-context, -W).
const FUNCTIONS_C: &str = r#"#include <stdio.h>

int add(int a, int b) {
    int result;
    result = a + b;
    return result;
}

int multiply(int a, int b) {
    int result;
    result = a * b;
    return result;
}

int subtract(int a, int b) {
    int result;
    result = a - b;
    return result;
}

int divide(int a, int b) {
    if (b == 0) {
        return -1;
    }
    return a / b;
}

int main() {
    int x = add(5, 3);
    int y = multiply(4, 2);
    printf("Results: %d, %d\n", x, y);
    return 0;
}
"#;

/// Prose text - tests word-level diffs
/// (--word-diff, --word-diff-regex, --color-words).
const PROSE: &str = r#"The quick brown fox jumps over the lazy dog.
This sentence contains multiple words that can be individually modified.
Word-level diffs highlight exactly which words changed between versions.

Lorem ipsum dolor sit amet, consectetur adipiscing elit.
Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.
Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.

The rain in Spain stays mainly in the plain.
How much wood would a woodchuck chuck if a woodchuck could chuck wood.
Peter Piper picked a peck of pickled peppers.
"#;

/// Binary-like content - tests binary handling
/// (--binary, --text, -a, --numstat with binary).
const BINARY: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk start
    0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, // 16x16 dimensions
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x91, 0x68, // bit depth, color type
    0x36, 0x00, 0x00, 0x00, 0x01, 0x73, 0x52, 0x47, // sRGB chunk
    0x42, 0x00, 0xAE, 0xCE, 0x1C, 0xE9, 0x00, 0x00, // more PNG data
    0x00, 0x04, 0x67, 0x41, 0x4D, 0x41, 0x00, 0x00, // gAMA chunk
    0xB1, 0x8F, 0x0B, 0xFC, 0x61, 0x05, 0x00, 0x00, // gamma value
];

/// Unicode content - tests encoding options (--encoding, multibyte handling).
const UNICODE: &str = r#"English: Hello, World!
Chinese: 你好世界
Japanese: こんにちは世界
Korean: 안녕하세요 세계
Russian: Привет мир
Arabic: مرحبا بالعالم
Greek: Γειά σου Κόσμε
Hebrew: שלום עולם
Emoji: 🚀 🌍 🎉 ✨ 💻
Math: ∑∏∫∂√∞≠≈
Symbols: © ® ™ € £ ¥ ¢
"#;

/// Similar content A - paired with B for algorithm comparison
/// (--histogram, --patience, --minimal, --diff-algorithm).
const SIMILAR_A: &str = r#"function setup() {
    initialize();
    configure();
}

function processA() {
    validate();
    transform();
    save();
}

function processB() {
    validate();
    transform();
    save();
}

function cleanup() {
    finalize();
    close();
}
"#;

/// Similar content B - paired with A for algorithm comparison.
const SIMILAR_B: &str = r#"function setup() {
    initialize();
    configure();
    prepare();
}

function processA() {
    check();
    validate();
    transform();
    save();
}

function processC() {
    validate();
    convert();
    save();
}

function cleanup() {
    finalize();
    close();
}
"#;

/// Generate a large file (~10KB) for testing size-related options.
fn generate_large() -> String {
    let line = "This is line number XXXX of the large test file for size demonstrations.\n";
    let mut content = String::with_capacity(11000);
    for i in 1..=150 {
        content.push_str(&line.replace("XXXX", &format!("{i:04}")));
    }
    content
}

/// Write all pre-generated fixtures to the sandbox work directory.
pub(super) fn write_fixtures(work_dir: &Path) -> Result<()> {
    let dir = work_dir.join(FIXTURES_DIR);
    fs::create_dir_all(&dir).context("create _fixtures directory")?;

    fs::write(dir.join("repeated.txt"), REPEATED).context("write repeated.txt")?;
    fs::write(dir.join("indented.txt"), INDENTED).context("write indented.txt")?;
    fs::write(dir.join("moveable.txt"), MOVEABLE).context("write moveable.txt")?;
    fs::write(dir.join("whitespace.txt"), WHITESPACE).context("write whitespace.txt")?;
    fs::write(dir.join("crlf.txt"), CRLF).context("write crlf.txt")?;
    fs::write(dir.join("functions.c"), FUNCTIONS_C).context("write functions.c")?;
    fs::write(dir.join("prose.txt"), PROSE).context("write prose.txt")?;
    fs::write(dir.join("binary.bin"), BINARY).context("write binary.bin")?;
    fs::write(dir.join("unicode.txt"), UNICODE).context("write unicode.txt")?;
    fs::write(dir.join("similar_a.txt"), SIMILAR_A).context("write similar_a.txt")?;
    fs::write(dir.join("similar_b.txt"), SIMILAR_B).context("write similar_b.txt")?;
    fs::write(dir.join("large.txt"), generate_large()).context("write large.txt")?;
    fs::write(dir.join("empty.txt"), "").context("write empty.txt")?;

    Ok(())
}
