---
title: Listing the models of an endpoint
summary: How the models an endpoint offers are listed, why Cline is asked elsewhere, and where the answers are kept.
refs: src/agent/models.rs, fetch, src/tui/modal/model_picker.rs, fetch_models, src/tui/state.rs, ModelCatalog
---

`src/agent/models.rs` is the one request that lists what an endpoint offers: `fetch` asks `GET <base>/models` and takes the `data[].id` of the answer. A Cline membership is the exception, since `Provider::models_path` points it at `ai/cline/recommended-models`, where the plan's models are the `clinePass[].id` of the answer.

The ids are sorted and become the choices of the model picker, kept in `AppState`'s `ModelCatalog` and filled in by `AppSignal::ModelsFetched`. The lists are fetched at startup for HTTP endpoints only (a `claude-code` endpoint takes its list from the configuration), and again when the picker opens for one whose last listing failed, so an endpoint that was down does not leave the picker empty for the rest of the run.
