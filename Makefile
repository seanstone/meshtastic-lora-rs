WASM_TARGET = wasm32-unknown-unknown
WASM_OUT    = dist
BIN_OUT     = bin

# Default target — build native + wasm GUI.
build: native wasm-web ## Build the native binary and the wasm GUI bundle.

# ── Build ────────────────────────────────────────────────────────────────────

native: ## Build the native mesh binary (release) and copy it to bin/.
	cargo build --release --bin mesh
	@mkdir -p $(BIN_OUT)
	cp target/release/mesh $(BIN_OUT)/mesh
	@echo "✓ mesh ready in $(BIN_OUT)/mesh"

wasm-web: ## Build the wasm GUI bundle into dist/ for `mesh` to serve.
	cargo build --target $(WASM_TARGET) --bin mesh_web \
		--no-default-features --features wasm --release
	@mkdir -p $(WASM_OUT)
	wasm-bindgen \
		target/$(WASM_TARGET)/release/mesh_web.wasm \
		--out-dir $(WASM_OUT) \
		--target web \
		--no-typescript
	cp web/index.html $(WASM_OUT)/index.html
	@echo "✓ mesh_web ready in $(WASM_OUT)/"

wasm-web-opt: wasm-web ## Apply wasm-opt size optimization to the GUI bundle.
	wasm-opt -Oz $(WASM_OUT)/mesh_web_bg.wasm -o $(WASM_OUT)/mesh_web_bg.wasm
	@echo "✓ wasm-opt applied"

# ── Run ──────────────────────────────────────────────────────────────────────

run: ## Run the mesh server (desktop GUI if a display is available).
	cargo run --release --bin mesh

run-headless: ## Run the server, skipping the desktop window.
	cargo run --release --bin mesh -- --headless

# ── Cleanup ──────────────────────────────────────────────────────────────────

clean-wasm: ## Remove the wasm GUI bundle.
	rm -rf $(WASM_OUT)

clean-bin: ## Remove the copied native binary.
	rm -rf $(BIN_OUT)

.PHONY: build native run run-headless wasm-web wasm-web-opt clean-wasm clean-bin
