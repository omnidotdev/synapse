# Synapse ONNX routing model

A learned routing strategy: the model predicts the **required quality** (a single
scalar in `0.0..1.0`) for a query, and the gateway routes to the cheapest
capability-satisfying model whose quality clears that bar. Predicting a target
rather than a model class keeps the model decoupled from the model registry, so
adding or removing models never silently misaligns the output.

## Feature contract

The 8-feature input vector must stay aligned with `profile_to_features` in
`../src/strategy/onnx.rs` (same order, same normalization):

| idx | feature | encoding |
| --- | --- | --- |
| 0 | input tokens | `estimated_input_tokens / 100_000` |
| 1 | task type | ordinal `0..5` (simple_qa, general, creative, analysis, code, math) |
| 2 | complexity | ordinal `0..2` (low, medium, high) |
| 3 | requires tool use | `0` or `1` |
| 4 | vision | `0` or `1` |
| 5 | long context | `0` or `1` |
| 6 | message count | `message_count / 50` |
| 7 | has system prompt | `0` or `1` |

Model I/O: one `[1, 8]` float input named `input`, one float output (the scalar
required quality).

## Train

```sh
python -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt

# Bootstrap (synthetic, distilled from the heuristic routing signals)
python train.py --out model.onnx

# Retrain on real outcomes once telemetry exists (see clickhouse_schema.sql)
python train.py --telemetry rows.csv --out model.onnx
```

The bootstrap model ships a usable router before any traffic exists, but its
quality is capped at the heuristics it distills. The real gains come from
retraining on `synapse.routing_telemetry`, where the label is the lowest model
quality that still produced an accepted answer.

## Deploy

The strategy is gated behind the `onnx` Cargo feature **and** is dormant unless
selected, so it never affects a default build:

1. Build the gateway with the feature: `cargo build --release --features onnx`
   (the ONNX Runtime native library is linked via the `ort` crate).
2. Point config at the model and select the strategy, in `synapse.toml`:

   ```toml
   [routing]
   strategy = "onnx"

   [routing.onnx]
   model_path = "/etc/synapse/routing-model.onnx"
   ```

If the feature is not compiled in, or the model fails to load, the strategy is
left unregistered and routing surfaces an "unknown strategy" error rather than
panicking. Inference also falls back to the best available model whenever no
model clears the predicted quality bar.
