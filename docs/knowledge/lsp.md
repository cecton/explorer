---
title: Code intelligence: the rust-analyzer client
summary: The rust-analyzer client: what it asks and never asks, which file does what, and why `highlight.rs` is outside it.
refs: src/lsp/mod.rs, src/lsp/rpc.rs, src/lsp/worker.rs, src/lsp/tokens.rs, src/lsp/symbols.rs, src/highlight.rs, src/loader.rs, src/loader/watch.rs, send_file_request
---

`src/lsp/` is a rust-analyzer client that serves reading only. It asks for semantic tokens (to color Rust files), hover (to name the symbol under the cursor) and references (to find every use of it), and nothing else: no diagnostics, no completion, no signature help, no rename. Of the server's own requests only `window/workDoneProgress/create` and `workspace/diagnostic/refresh` are answered, with `null`, so the server stops retrying them; any other is ignored.

`mod.rs` holds the request types and the senders (plus the buffer for early requests and the UTF-16 offset helpers), `rpc.rs` the `Content-Length` framing and the file URIs, `worker.rs` the server process, its warmup and its restarts, `tokens.rs` the semantic-token round trip, `symbols.rs` the hover-then-references round trip.

The colors themselves live outside this module, in `src/highlight.rs`, and that placement is what breaks a type cycle: `highlight` imports nothing from the crate, so the loader can hold colored lines without naming `lsp`, even though only `lsp/tokens.rs` ever writes them. `loader/watch.rs` is the one place outside the UI that asks for them, through `lsp::send_file_request`.

Only Rust files get colors and references, since only rust-analyzer is wired in. `EXPLORER_NO_LSP=1` starts no server at all: a run with it looks the same, minus the colors and the symbol highlights, which is why running the app without needing them saves a rust-analyzer per instance.
