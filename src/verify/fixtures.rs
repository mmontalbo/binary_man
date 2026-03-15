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
/// Designed with adversarial interleaving that triggers different hunk
/// boundaries between Myers (greedy) and patience/histogram algorithms.
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

section_start {
    process_item zeta
    validate zeta
    save zeta
}
section_end

section_start {
    process_item eta
    validate eta
    save eta
}
section_end

section_start {
    process_item theta
    validate theta
    save theta
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

# Unique anchors for patience algorithm
# Patience uses these unique lines to align the diff,
# producing cleaner hunks than Myers on repeated content.
=== configuration block A ===
    key = value_a
    mode = active
    retry = 3

=== configuration block B ===
    key = value_b
    mode = standby
    retry = 5

=== configuration block C ===
    key = value_c
    mode = active
    retry = 3

=== configuration block D ===
    key = value_d
    mode = standby
    retry = 5
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
/// Large enough (8 functions, ~130 lines) that --function-context shows
/// a visible difference from the default 3-line context.
const FUNCTIONS_C: &str = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Parse a configuration value from a string.
   Returns the integer value or -1 on error. */
int parse_config(const char *input) {
    if (input == NULL) {
        return -1;
    }
    int value = atoi(input);
    if (value < 0 || value > 1000) {
        fprintf(stderr, "config value out of range: %d\n", value);
        return -1;
    }
    return value;
}

/* Initialize the processing buffer with default values.
   The buffer must be pre-allocated by the caller. */
void init_buffer(int *buffer, int size) {
    for (int i = 0; i < size; i++) {
        buffer[i] = 0;
    }
    buffer[0] = 1;  /* sentinel value */
}

/* Validate that all buffer entries are within range.
   Returns 1 if valid, 0 otherwise. */
int validate_buffer(const int *buffer, int size) {
    if (buffer == NULL || size <= 0) {
        return 0;
    }
    for (int i = 0; i < size; i++) {
        if (buffer[i] < -1000 || buffer[i] > 1000) {
            return 0;
        }
    }
    return 1;
}

/* Transform buffer values using the given multiplier.
   Applies the transformation in-place. */
void transform_buffer(int *buffer, int size, int multiplier) {
    for (int i = 0; i < size; i++) {
        buffer[i] = buffer[i] * multiplier;
        if (buffer[i] > 1000) {
            buffer[i] = 1000;
        }
    }
}

/* Compute a running average over a window of values.
   Returns the average of the last 'window' entries. */
double running_average(const int *buffer, int size, int window) {
    if (size == 0 || window <= 0) {
        return 0.0;
    }
    int start = size - window;
    if (start < 0) {
        start = 0;
    }
    double sum = 0.0;
    int count = 0;
    for (int i = start; i < size; i++) {
        sum += buffer[i];
        count++;
    }
    return sum / count;
}

/* Merge two sorted buffers into a destination buffer.
   Both source buffers must already be sorted. */
void merge_sorted(const int *a, int a_len,
                  const int *b, int b_len,
                  int *dest) {
    int i = 0, j = 0, k = 0;
    while (i < a_len && j < b_len) {
        if (a[i] <= b[j]) {
            dest[k++] = a[i++];
        } else {
            dest[k++] = b[j++];
        }
    }
    while (i < a_len) {
        dest[k++] = a[i++];
    }
    while (j < b_len) {
        dest[k++] = b[j++];
    }
}

/* Format a buffer as a comma-separated string.
   Caller must free the returned string. */
char *format_buffer(const int *buffer, int size) {
    char *result = malloc(size * 12);
    if (result == NULL) {
        return NULL;
    }
    result[0] = '\0';
    for (int i = 0; i < size; i++) {
        char tmp[16];
        snprintf(tmp, sizeof(tmp), "%s%d", (i > 0 ? ", " : ""), buffer[i]);
        strcat(result, tmp);
    }
    return result;
}

/* Print a summary report of the buffer contents.
   Shows min, max, average, and total entries. */
void print_summary(const int *buffer, int size) {
    if (size == 0) {
        printf("Empty buffer\n");
        return;
    }
    int min = buffer[0], max = buffer[0];
    long total = 0;
    for (int i = 0; i < size; i++) {
        if (buffer[i] < min) min = buffer[i];
        if (buffer[i] > max) max = buffer[i];
        total += buffer[i];
    }
    printf("Summary: %d entries, min=%d, max=%d, avg=%.1f\n",
           size, min, max, (double)total / size);
}

int main(int argc, char *argv[]) {
    int config = parse_config(argc > 1 ? argv[1] : "10");
    int buffer[100];
    init_buffer(buffer, config);
    transform_buffer(buffer, config, 3);
    if (validate_buffer(buffer, config)) {
        print_summary(buffer, config);
    }
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
