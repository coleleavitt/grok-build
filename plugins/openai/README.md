# openai - GPT model provider plugin

Contributes OpenAI GPT models to the Grok model catalog via the
`modelProviders` manifest field. Once installed, `gpt-5.5`, `gpt-5.5-pro`,
and the GPT-5.4 family appear in the model picker and route to the OpenAI
Responses API (`https://api.openai.com/v1/responses`).

## Install

User scope (always trusted, applies everywhere):

```sh
mkdir -p ~/.grok/plugins
ln -s "$(pwd)/plugins/openai" ~/.grok/plugins/openai
```

Project scope: copy or symlink into `<repo>/.grok/plugins/openai`
(requires granting the project plugin trust on first load).

## Auth

Set `OPENAI_API_KEY`. If you use `~/.env.d/openai`, make sure that file exports
that name. The provider declares `authScheme: bearer`, so the key is sent as an
`Authorization: Bearer ...` header.

## Notes

- The plugin uses the existing OpenAI-compatible Responses backend; no separate
  runtime client is needed.
- The GPT-5.6 preview series is intentionally not listed.
