# Contributing to Renderly

Thank you for helping. This guide covers the mechanics. The rules for the code itself are in
[AGENTS.md](AGENTS.md); read it before your first change. They apply to people and to AI agents
alike.

## Set up

You need Node.js 22 or later and pnpm 10 (`corepack enable` installs the version pinned in
`package.json`).

```sh
pnpm install
pnpm dev        # spike page at http://localhost:5173, cross-origin isolated
pnpm test       # unit tests
pnpm typecheck
pnpm lint
pnpm format     # rewrite code with Prettier
```

Use Chrome or Edge for development. Safari and Firefox are best-effort targets.

## Making a change

- Keep a pull request to one thing. Work that follows a planned slice names the slice.
- Tests come with the change. CI runs typecheck, lint, formatting, the token check, tests and a
  build, and all of them must pass.
- Interface changes follow [docs/design-system/README.md](docs/design-system/README.md) and are
  checked in the dark and the light theme.
- A change that settles a design question adds a record in [docs/decisions/](docs/decisions/).
- New dependencies need a reason in the pull request and a licence compatible with Apache-2.0.
  GPL and AGPL code is not accepted.
- Do not name other video editors anywhere in the repository. Do not copy code, shaders, LUTs,
  fonts, icons or other assets from other products.

## Sign your commits (DCO)

Renderly uses the [Developer Certificate of Origin](https://developercertificate.org/) instead of
a contributor licence agreement. By signing off a commit you state that you wrote it, or
otherwise have the right to submit it under the project's licence.

Add the sign-off with `-s`:

```sh
git commit -s -m "Add the decode worker"
```

This appends a line with the name and email from your git configuration:

```
Signed-off-by: Your Name <you@example.com>
```

Every commit in a pull request needs it. To sign off commits you have already made, run
`git rebase --signoff main` and force-push your branch.

## Licence

Contributions are licensed under the [Apache License 2.0](LICENSE), the licence of the project.
Packs have their own licence allow-list, described in the pack authoring guide.
