# Speech to Text (`ai.stt`)

AI · version 1 · cook budget 150 µs

Transcribes speech from an audio input with Whisper (faster-whisper) on an AI Host. The input is gated by level: what is said is sent once the speaker pauses, and the transcript arrives asynchronously on the DAT `text` output (wire it into LLM Chat's `prompt` input or a Prompt Template); it is also published under `Publish As` for `Prompt From`. Channels: `active` while speech is heard, `busy` while transcribing, `new_text` for one frame when a transcript arrives, `count`, `level` and `ready` (a host has the model loaded). Needs a Whisper model from the model manager and the host's `stt` extra; `builtin.stt_echo` runs anywhere for testing.

Cooks every frame (time-dependent).

Sink: requested every frame even when nothing downstream is viewed, because it has external side effects. Like any node it cooks only when dirty: an input, a parameter or a bound channel changed, it is time-dependent, or it asked to cook again.

## Inputs

| Name | Type | Notes |
|---|---|---|
| `audio` | Sig |  |

## Outputs

| Name | Type | Notes |
|---|---|---|
| `out` | Sig |  |
| `text` | Dat |  |

## Parameters

| Name | Label | Type | Default | Range / options | Description |
|---|---|---|---|---|---|
| `route` | Route | string | `"local"` |  | Where the node runs, in order of preference: `local`, `lan:<name>` and `cloud:<name>` separated by `→`, `->` or commas. The first ready host with the `stt.whisper` adapter wins. Cloud hosts are skipped in Sovereign mode. |
| `model` | Model | string | `"whisper-base"` |  | Model id from the model manager (`whisper-base`, `whisper-small`), a path to a faster-whisper model folder, or `builtin.stt_echo` (describes the audio instead of transcribing it; for tests). |
| `language` | Language | string | `""` |  | Spoken language as an ISO code (`en`, `de`, `fr`, …); empty detects it for each utterance. |
| `task` | Task | menu | `"transcribe"` | `transcribe`, `translate` | `transcribe` writes what was said; `translate` writes it in English. |
| `prompt` | Vocabulary | string | `""` |  | Names and words to expect (Whisper's initial prompt), for example the performers' names. |
| `threshold` | Threshold | float | `0.02` | 0 – 1 | Input level (RMS) above which the input counts as speech. Watch the `level` channel while speaking and set it between the room and the voice. |
| `hang_ms` | Hang | float | `600.0` | 50 – 5000 | Milliseconds below the threshold that end an utterance and send it. |
| `min_ms` | Minimum | float | `250.0` | 0 – 5000 | Utterances shorter than this many milliseconds (clicks, coughs) are dropped. |
| `max_seconds` | Maximum | int | `15` | 1 – 30 | Longest utterance in seconds: longer speech is sent in pieces of this length. |
| `publish` | Publish As | string | `"speech"` |  | The name each transcript is also published under, for LLM Chat's `Prompt From`. Empty: not published (the `text` output still has it). |

## Platforms

All platforms.

## Examples

- [Speech and chat](../../../examples/speech-chat.lasair/) — `speech`
