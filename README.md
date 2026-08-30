# runvault

Plain-file experiment tracking for reproducible research.

One run is one directory. The directory is the source of truth; every tracking
UI on top of it (DuckDB index, MLflow, an Obsidian dashboard) is an optional,
removable index layer. Experiment code depends on `runvault` alone.

`runvault` is designed to record simulation (ABM), LLM safety evaluation and
anomaly-detection experiments in the *same* shape, so that runs can be compared
across repositories and across years.

## Status

Phase 0 (frozen schemas) is complete. The Rust crate is not implemented yet.

## Layout

| Path | Contents |
| --- | --- |
| `schema/v1/` | Frozen JSON Schemas, the flattened index columns, and the core vocabulary. These files are the specification. |
| `tools/check_design_doc.py` | Validates every example in the design note against the schemas. |
| `tools/test_schemas.py` | Positive/negative cases pinning down what the schemas must accept and reject. |

The design note lives in the author's Obsidian vault:
`設計書/rs-runvault_実験管理基盤設計書.md`.

## Checks

```bash
uv run --with jsonschema python tools/test_schemas.py
uv run --with jsonschema python tools/check_design_doc.py
```

## License

TBD.
