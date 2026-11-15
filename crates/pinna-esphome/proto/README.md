# Vendored ESPHome API schema

Fetched unmodified from the ESPHome repository at commit
`fb65096ea3dc4cccf53681b501dec01fd2ec9538` (`fb65096e`, the schema LibreEcho was
tested against), path `esphome/components/api/`.

| file                | sha256                                                             |
| ------------------- | ------------------------------------------------------------------ |
| `api.proto`         | `35258fa3956e3701ed091d48fec3ea50a15b285faa7c8613c267b5ab1e75541b` |
| `api_options.proto` | `5a9f2741ec0cfc613b4792de1f9d2d569207d6cfbe69fb52116310b35f745954` |

`build.rs` compiles them with `protox` (pure Rust, no `protoc`) and `prost-build`,
and derives the message-id table from the `(id)` message option.

License: ESPHome publishes everything except its C/C++ runtime sources under the
MIT license (see the repository LICENSE); these `.proto` files are MIT.
