# anthropic — Claude model provider plugin

Contributes Anthropic Claude models to the Grok model catalog via the
`modelProviders` manifest field. Once installed, `claude-opus-4-8`,
`claude-sonnet-5`, `claude-haiku-4-5`, and `claude-fable-5` appear in the
model picker alongside the built-in models and route to the Anthropic
Messages API (`https://api.anthropic.com/v1/messages`).

## Install

User scope (always trusted, applies everywhere):

```sh
mkdir -p ~/.grok/plugins
ln -s "$(pwd)/plugins/anthropic" ~/.grok/plugins/anthropic
```

Project scope: copy or symlink into `<repo>/.grok/plugins/anthropic`
(requires granting the project plugin trust on first load).

## Auth

Set `ANTHROPIC_API_KEY`. The provider declares `authScheme: x_api_key`, so
the key is sent as the `x-api-key` header with
`anthropic-version: 2023-06-01`, matching the first-party API contract.

## Notes

- Reasoning effort is supported: the Messages backend maps Grok effort
  levels to `output_config.effort` with adaptive thinking — no deprecated
  `budget_tokens`, so current-generation Claude models accept it.
- No `temperature`/`top_p` are declared; Claude 4.7+ models reject sampling
  parameters, and the sampler only sends them when a model sets them.
- OAuth / subscription-account auth (opencode-anthropic-auth style) is not
  part of this manifest. That class of behavior plugs in through
  `providerRequestAdapter` (per provider or per model): the `command`
  variant runs an executable that receives `{endpoint, model, headers,
  body}` on stdin and returns replacement headers/body on stdout, e.g. to
  inject a rotated bearer token per request.
