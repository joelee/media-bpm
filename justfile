# Keeps quoted arguments intact, so file names with spaces survive.
set positional-arguments

[private]
default:
    @just --list

# Analyze media files, e.g. just run "My Song.mp3" --max-bpm 100
run *args:
    @cargo run --release --quiet -- "$@"

# Build the optimised binary
build:
    cargo build --release

# Run the tests, optionally only those matching a filter
test *args:
    cargo test "$@"

# Format the code
fmt:
    cargo fmt

# Run clippy, treating warnings as errors
lint:
    cargo clippy --all-targets -- -D warnings

# Everything that should pass before a commit
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Install media-bpm into ~/.cargo/bin
install:
    cargo install --path .

# Remove build output
clean:
    cargo clean
