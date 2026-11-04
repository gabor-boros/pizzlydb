# pizzlydb-auth

Identity, permissions, and policy epochs.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-auth` will identify a principal and decide what that principal may do. Authorization runs before query planning and before result delivery. Durable resume tokens bind the tenant, the permissions, and the policy epoch so a token cannot be replayed under another authority.

The crate depends on `pizzlydb-core`. It does not infer application authorization from graph reachability.

## Modules

- `identity` — principals, including service credentials
- `permission` — namespace and administrative permissions
- `policy` — policy epochs stored with resume tokens

## License

Licensed under the [Apache License 2.0](../../LICENSE).
