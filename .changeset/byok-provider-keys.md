---
"synapse": patch
---

Run BYOK requests on the caller's own provider key

A BYOK request is not metered, but every modality sent it upstream on the gateway's configured provider key: LLM providers only honored the resolved key when `forward_authorization` was set (it never is), and embeddings, speech-to-text, and text-to-speech always used the configured key. A BYOK request was therefore served on the gateway's account at no cost to the caller. Now:

- LLM providers always use the key resolved for the request: the caller's BYOK key, or the managed key for a managed request. Requests without a billing identity (billing off, self-host) and managed requests for a provider without a managed key keep the configured key
- failover and cascade escalation resolve the key again for each provider they try, so a key is only ever sent to the provider it belongs to. A BYOK caller without a key for an alternate provider skips that alternate
- embeddings, speech-to-text, and text-to-speech run a BYOK request on the caller's key for the target provider; managed requests and requests without a billing identity keep the configured key
- a BYOK request for a provider the caller has no key for is refused with `400` before any upstream call, on every modality
- a BYOK request to an AWS Bedrock provider is refused with `400`, since Bedrock authenticates with the gateway's AWS credentials and cannot carry a caller key
- `forward_authorization` on an LLM provider is deprecated and has no effect. It still parses so existing configs keep loading, and the gateway logs a warning when it is set
