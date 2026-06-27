-- Routing telemetry for retraining the ONNX routing model.
--
-- The gateway logs one row per routed request: the 8-feature vector it derived
-- (identical to profile_to_features in onnx.rs), the model it chose, and the
-- realized outcome. Retraining (see train.py --telemetry) derives the
-- `required_quality` label as the lowest model quality that still produced an
-- accepted answer for queries of this shape.

CREATE TABLE IF NOT EXISTS synapse.routing_telemetry
(
    ts                  DateTime64(3) DEFAULT now64(),
    request_id          String,

    -- Feature vector, same order and normalization as profile_to_features
    norm_input_tokens   Float32,
    task_type           UInt8,   -- ordinal 0..5
    complexity          UInt8,   -- ordinal 0..2
    requires_tool_use   UInt8,   -- 0 or 1
    vision              UInt8,   -- 0 or 1
    long_context        UInt8,   -- 0 or 1
    norm_message_count  Float32,
    has_system_prompt   UInt8,   -- 0 or 1

    -- Decision and outcome
    chosen_provider     String,
    chosen_model        String,
    chosen_quality      Float32, -- registry quality of the chosen model
    strategy            LowCardinality(String),

    -- Outcome signal used to derive the training label. `accepted` is whether
    -- the answer met the quality bar (downstream judge, user feedback, or no
    -- retry/regeneration). `escalated` marks a cascade escalation, i.e. the
    -- chosen model was too weak
    accepted            UInt8,
    escalated           UInt8,
    latency_ms          UInt32
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
ORDER BY (ts, request_id)
TTL toDateTime(ts) + INTERVAL 180 DAY;
