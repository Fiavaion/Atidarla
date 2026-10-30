# LLM Chat (`ai.llm_chat`)

AI · version 1 · cook budget 50 µs

Sends a prompt to a language model and publishes the reply under `Publish As` (DAT text out when the engine routes DAT). Ollama on this machine by default (`qwen2.5-coder:7b`); an OpenAI-compatible server (llama.cpp, LM Studio, vLLM) or Anthropic opt in. Sends on the `Send` pulse, a rising `trigger` input, each new text under `Prompt From` (a Speech to Text node's transcripts, say) or, with `On Change`, an edited prompt. Asynchronous: channels `busy`, `new_reply` (one frame), `replies` and `error`. Every request goes through Sovereign mode's egress gate: Anthropic and internet endpoints are refused while it is on.

Cooks every frame (time-dependent).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `trigger` | Sig | optional |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |
| `reply` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `provider` | Provider | menu | `"ollama"` | `ollama`, `openai_compat`, `anthropic` | Who answers: `ollama` (local, the default), `openai_compat` (any server with `/v1/chat/completions`; `OPENAI_API_KEY` from the environment if set) or `anthropic` (internet; needs `ANTHROPIC_API_KEY` in the environment and Sovereign mode off). |
| `endpoint` | Endpoint | string | `""` |  | Base URL of the server; empty: the provider's default (`http://127.0.0.1:11434` for Ollama, `http://127.0.0.1:8080` for OpenAI-compatible, `https://api.anthropic.com`). A LAN address is allowed in Sovereign mode; an internet address is not. |
| `model` | Model | string | `""` |  | Model name; empty: the provider's default (`qwen2.5-coder:7b` on Ollama, `claude-haiku-5-5` on Anthropic, whatever the OpenAI-compatible server loaded). |
| `system` | System Prompt | string | `"You are part of a live visual performance. Answer in one short sentence."` |  | Instructions the model follows for every prompt. |
| `prompt` | Prompt | string | `""` |  | The prompt. With `Prompt From` set, it is put before the incoming text (an instruction such as "Turn this into an image prompt:"). |
| `prompt_from` | Prompt From | string | `""` |  | The `Publish As` name of another node (Speech to Text, LLM Chat): each new text it publishes is sent. Empty: only the prompt above. |
| `send` | Send | pulse | `false` |  | Sends the prompt now. |
| `on_change` | On Change | toggle | `false` |  | Sends whenever the prompt changes (for a prompt driven by an expression or a mapping). |
| `temperature` | Temperature | float | `0.7` | 0 – 2 | Randomness of the reply: 0 is the most predictable. |
| `max_tokens` | Max Tokens | int | `256` | 1 – 8192 | Longest reply, in tokens. |
| `history` | History | int | `0` | 0 – 20 | Earlier exchanges sent with each prompt, so the model remembers the conversation; 0 sends each prompt on its own. Changing a setting above starts a new conversation. |
| `timeout_s` | Timeout | float | `60.0` | 1 – 600 | Seconds a request may take before it fails. |
| `publish` | Publish As | string | `"reply"` |  | The name each reply is published under, for another node's `Prompt From` and the app. Empty: not published. |

## Platforms

All platforms.

## Examples

- [Speech and chat](../../../examples/speech-chat.atidarla/) — `chat`
