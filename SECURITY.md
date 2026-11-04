# Security policy

Report a vulnerability through [GitHub Security Advisories](https://github.com/gabor-boros/pizzlydb/security/advisories/new).

`cargo audit` runs in CI on pull requests and on pushes to `main`. The release workflow runs it again on the release tag. Release archives are uploaded only after that audit succeeds.
