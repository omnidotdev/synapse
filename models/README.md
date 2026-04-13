# ONNX Routing Model

The ONNX routing strategy uses a pre-trained classifier to select the optimal model for each request. Training happens offline; inference runs in Rust via the `ort` crate.

## Input Schema

The model accepts an 8-feature float32 tensor with shape `(1, 8)`:

| Index | Feature                | Encoding                                                    |
| ----- | ---------------------- | ----------------------------------------------------------- |
| 0     | `estimated_input_tokens` | Divided by 100,000                                        |
| 1     | `task_type`            | Ordinal: SimpleQa=0, General=1, Creative=2, Analysis=3, Code=4, Math=5 |
| 2     | `complexity`           | Ordinal: Low=0, Medium=1, High=2                            |
| 3     | `requires_tool_use`    | Boolean: 0.0 or 1.0                                         |
| 4     | `vision`               | Boolean: 0.0 or 1.0 (from `required_capabilities`)          |
| 5     | `long_context`         | Boolean: 0.0 or 1.0 (from `required_capabilities`)          |
| 6     | `message_count`        | Divided by 50                                                |
| 7     | `has_system_prompt`    | Boolean: 0.0 or 1.0                                         |

## Output

A probability distribution (float32 tensor) over model profiles, where each index corresponds to a profile in the model registry. The class with the highest probability is selected, subject to capability filtering.

## Training Data Format

Each training sample is a row with 8 input features and a label column:

```csv
input_tokens,task_type,complexity,tool_use,vision,long_context,message_count,system_prompt,selected_model
0.005,4.0,2.0,1.0,0.0,0.0,0.06,1.0,0
0.001,0.0,0.0,0.0,0.0,0.0,0.02,0.0,2
```

- Input features use the same encoding described above
- `selected_model` is the integer class label matching the profile index in the registry

## Training Workflow

1. **Collect routing decisions** — log each request's feature vector alongside the model that was ultimately selected (or the model that produced the best user outcome)
2. **Train classifier** — use a standard multi-class classifier (e.g. scikit-learn `MLPClassifier`, XGBoost, or PyTorch) on the collected dataset
3. **Export to ONNX** — convert the trained model using `skl2onnx`, `torch.onnx.export`, or the framework's ONNX exporter
4. **Validate** — run the exported model against a held-out test set to verify accuracy
5. **Deploy** — place the `.onnx` file at `models/router.onnx` and enable the `onnx` feature flag

## Expected Model File

```
models/router.onnx
```

The gateway loads this file at startup via `OnnxStrategy::load("models/router.onnx")`. If the file is missing or the `onnx` feature is disabled, the strategy returns an error and the system falls back to the configured heuristic strategy.
