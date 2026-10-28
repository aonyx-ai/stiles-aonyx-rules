# Changesets

Each file in this directory is a changeset: one change that the next release
of the rules ships. A pull request that makes a change that users notice adds
a changeset for it. A new rule, a rule that reports more or less, and a move
of the stiles pin are such changes.

A changeset is a Markdown file with TOML front matter between two lines of
`+++`, and nothing below it:

```markdown
+++
category = "Added"
title = "`lint.todo-comment` reports a `TODO` comment"
+++
```

- `category` is one of the categories of [Keep a Changelog]: `Added`,
  `Changed`, `Deprecated`, `Removed`, `Fixed`, or `Security`.
- `title` is a short description of the change. It becomes the entry in the
  changelog.

`otto create-changeset` writes a changeset. [Otto] reads only the files whose
name ends in `.md`, and it skips this `README.md`. A release turns the
changesets into a section of `CHANGELOG.md`.

[keep a changelog]: https://keepachangelog.com/en/1.1.0/
[otto]: https://github.com/aonyx-ai/otto
