---
"synapse": minor
---

Serve each product its model picker and default from the gateway

- `[billing.products.<app>]` declares the chat models a product-scoped key may use (`models`, as `provider/model`) and its `default_model`. Every listed model must be priced in `[[llm.routing.models]]`, checked at boot
- `GET /v1/models` lists exactly the calling product's models in policy order; any other key keeps seeing every model. Each entry adds a `synapse` object: `display_name`, `default`, `context_window`, and `credits_per_mtok` (`input`, `output`), the retail price in $0.01 credits with the caller's margin, from the same pricing table the gateway bills with (`null` when the caller is not billed for the model)
- `model: "auto"` always works for a product with a policy: it resolves to the default, or routes among the product's own models when the product's tier has `smart_routing`. `fast`, `best`, and `cheap` without the entitlement resolve to the default too
- A product key requesting a model outside its policy gets a 400 `invalid_request_error`, before any provider call or spend. Failover and cascade escalation never leave the product's models, and billing follows the model that served
- `[[llm.routing.models]]` entries take an optional `display_name`
- Production: model policies for keystone, facet, beacon, creator, and myfi; priced Claude Sonnet 4.6; smart routing thresholds moved to Claude Haiku 5.5 and Claude Sonnet 5.5
