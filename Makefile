# ── Native ────────────────────────────────────────────────────────────────────

run:
	cargo run --bin mesh_sim

run-node:
	cargo run --bin mesh_node

run-node-uhd:
	cargo run --bin mesh_node -- --uhd --freq 906.875

run-gui-sim:
	cargo run --bin gui_sim

# ── WASM ──────────────────────────────────────────────────────────────────────

WASM_TARGET  = wasm32-unknown-unknown
WASM_OUT     = dist
WASM_PROFILE = release

wasm: ## Build WASM and prepare dist/
	cargo build --target $(WASM_TARGET) --bin mesh_sim \
		--no-default-features --features wasm --$(WASM_PROFILE)
	@mkdir -p $(WASM_OUT)
	wasm-bindgen \
		target/$(WASM_TARGET)/$(WASM_PROFILE)/mesh_sim.wasm \
		--out-dir $(WASM_OUT) \
		--target web \
		--no-typescript
	cp web/index.html $(WASM_OUT)/index.html
	@echo "✓ WASM build ready in $(WASM_OUT)/"

wasm-serve: wasm ## Build and serve locally
	@echo "Serving on http://localhost:3000"
	python3 -m http.server 3000 --directory $(WASM_OUT)

wasm-opt: wasm ## Build with wasm-opt size optimization
	wasm-opt -Oz $(WASM_OUT)/mesh_sim_bg.wasm -o $(WASM_OUT)/mesh_sim_bg.wasm
	@echo "✓ wasm-opt applied"

clean-wasm:
	rm -rf $(WASM_OUT)

.PHONY: run run-gui-sim wasm wasm-serve wasm-opt clean-wasm
