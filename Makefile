LOCAL_RUST_BIN := $(CURDIR)/build/toolchain/bin
CARGO ?= $(if $(wildcard $(LOCAL_RUST_BIN)/cargo),$(LOCAL_RUST_BIN)/cargo,cargo)
RUSTC ?= $(if $(wildcard $(LOCAL_RUST_BIN)/rustc),$(LOCAL_RUST_BIN)/rustc,rustc)
ZIRAN ?= ziran
export CARGO_HOME := $(CURDIR)/build/cargo-home
export CARGO_TARGET_DIR := $(CURDIR)/codex-rs/target
export XCODER_RUNTIME_DIR := $(CURDIR)/build/native-runtime
export RUSTC ZIRAN
export CARGO_BUILD_JOBS ?= 1
# UI plugins retain their separate JavaScript sandbox; tool execution is native.
UI_FEATURES ?=
CARGO_FEATURE_FLAGS := $(if $(UI_FEATURES),--features $(UI_FEATURES),)
BACKGROUND_BUILD := python3 scripts/low-priority-build.py

.PHONY: all native-test test runtime-check install
all:
	mkdir -p build/bin
	$(BACKGROUND_BUILD) $(CARGO) build --manifest-path codex-rs/Cargo.toml --release -p codex-cli -p codex-code-mode-host $(CARGO_FEATURE_FLAGS)
	python3 scripts/package-xcoder.py

native-test:
	mkdir -p build/native-dispatch
	$(ZIRAN) build --project --target=c -o build/native-dispatch native/dispatch.zi
	$(CC) -O3 -std=c99 -c build/native-dispatch/dispatch.c -I build/native-dispatch -o build/native-dispatch/dispatch.o
	$(AR) rcs build/native-dispatch/libxcoder_dispatch.a build/native-dispatch/dispatch.o
	$(BACKGROUND_BUILD) $(RUSTC) --edition=2024 --test tests/native_abi.rs -L build/native-dispatch -l static=xcoder_dispatch -o build/native-dispatch/native-abi-test
	build/native-dispatch/native-abi-test
	XCODER_ZIRAN_TOOLCHAIN="$$($(ZIRAN) pkg path ziran)" python3 tests/native_cell_test.py

runtime-check:
	$(BACKGROUND_BUILD) $(CARGO) check --manifest-path codex-rs/Cargo.toml -p codex-code-mode-runtime

test:
	$(BACKGROUND_BUILD) python3 scripts/test-xcoder.py --cargo "$(CARGO)" --ui-features "$(UI_FEATURES)"

install:
	python3 scripts/install-xcoder.py
