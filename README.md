# Translate

A free and private translator, with no accounts, ads or tracking.

[Try Translate](https://translate.avkean.com)

Translations are done by gpt-oss-120b through [Privatemode](https://www.privatemode.ai), which runs the model on confidential computing hardware. Requests are encrypted before they leave the server, so not even Privatemode can read them. Nothing is stored or logged.

## Running it

It runs as two containers: the app, a small Rust server, and Privatemode's proxy, which encrypts each request.

1. Copy `.env.example` to `.env` and add your Privatemode API key.
2. Run `docker compose up -d --build`.

The compose file expects an external Docker network called `proxy_net`, shared with the reverse proxy, which reaches the app on port 3000.

For development, run the proxy on its own with port 8080 published, then start the app with `UPSTREAM_URL=http://127.0.0.1:8080/v1/chat/completions cargo run`.

## License

MIT
