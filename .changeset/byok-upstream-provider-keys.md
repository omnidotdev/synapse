---
"synapse": patch
---

Look up BYOK keys by the vendor a provider calls, not its configured name

Speech-to-text looked up a BYOK caller's key under the gateway's provider name (`whisper`), but callers register keys under the vendor (`openai`), so every BYOK transcription was refused with a 400. Speech-to-text, text-to-speech, embeddings, and image generation now resolve the caller's key by the upstream vendor of the provider type (`openai` for Whisper, OpenAI TTS, embeddings, and images; `deepgram`; `elevenlabs`), whatever name the provider is configured under
