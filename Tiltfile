project_name = "synapse"

local_resource(
    "build-%s" % project_name,
    cmd="cargo build -p synapse",
    deps=[
        "synapse/src",
        "crates",
        "Cargo.toml",
    ],
    labels=[project_name],
)

local_resource(
    "dev-%s" % project_name,
    serve_cmd="cargo run -p synapse",
    resource_deps=["build-%s" % project_name],
    readiness_probe=probe(
        http_get=http_get_action(
            path="/health",
            port=6000,
        ),
        initial_delay_secs=5,
        period_secs=5,
    ),
    labels=[project_name],
)
