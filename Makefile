# ── Native ────────────────────────────────────────────────────────────────────

run: ## Run mesh server with desktop GUI (if a display is available).
	cargo run --bin mesh

run-headless: ## Run mesh server without launching the desktop window.
	cargo run --bin mesh -- --headless

run-headless-only: ## Pure-server build — no eframe linked. For deployment.
	cargo run --bin mesh --no-default-features --features server,uhd

# ── WASM ──────────────────────────────────────────────────────────────────────

WASM_TARGET  = wasm32-unknown-unknown
WASM_OUT     = dist
WASM_PROFILE = release

wasm-web: ## Build the WS-backed wasm GUI (mesh_web) into dist/ for `mesh` to serve.
	cargo build --target $(WASM_TARGET) --bin mesh_web \
		--no-default-features --features wasm --$(WASM_PROFILE)
	@mkdir -p $(WASM_OUT)
	wasm-bindgen \
		target/$(WASM_TARGET)/$(WASM_PROFILE)/mesh_web.wasm \
		--out-dir $(WASM_OUT) \
		--target web \
		--no-typescript
	cp web/index.html $(WASM_OUT)/index.html
	@echo "✓ mesh_web ready in $(WASM_OUT)/ — start the server with: make run"

wasm-web-opt: wasm-web ## Build mesh_web and apply wasm-opt size optimization.
	wasm-opt -Oz $(WASM_OUT)/mesh_web_bg.wasm -o $(WASM_OUT)/mesh_web_bg.wasm
	@echo "✓ wasm-opt applied"

clean-wasm:
	rm -rf $(WASM_OUT)

.PHONY: run run-headless run-headless-only wasm-web wasm-web-opt clean-wasm
