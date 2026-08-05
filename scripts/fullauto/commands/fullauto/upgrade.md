---
description: Ship phase — install the released Stella from the Homebrew tap and stop the local dev build from shadowing it.
argument-hint: "[--dry-run] [--no-dev-alias]"
---

# fullauto:upgrade — consume what you shipped

The delivery half of autonomous delivery. After a cycle lands, this makes the
`stella` on this machine **the released build a user would get**, not the dev
binary sitting in `target/release`.

```bash
scripts/fullauto.sh upgrade --dry-run    # show every edit, change nothing
scripts/fullauto.sh upgrade              # do it
```

Run `--dry-run` first the very first time. This is the one command in the family
that edits `~/.zshrc`.

---

## The two traps

**1. `brew install stella` installs an Atari 2600 emulator.**

Homebrew-core owns the name `stella` — it is a well-established VCS emulator with
hundreds of installs a year. `brew install stella` succeeds, exits 0, and puts
entirely unrelated software on the PATH. The only safe spelling is
tap-qualified:

```bash
brew install macanderson/stella/stella      # correct
brew upgrade macanderson/stella/stella      # correct
brew install stella                         # WRONG — Atari emulator, exit 0
```

The tap is `macanderson/stella` →
`https://github.com/macanderson/homebrew-tap.git`. The formula builds from
source at the release tag (`cargo install --locked`), so an upgrade needs a Rust
toolchain and takes a few minutes.

**2. There is no alias to remove — it is a PATH prepend.**

`~/.zshrc` carries:

```sh
export PATH="$HOME/Projects/stella/target/release:$PATH"
```

That prepend is what makes `stella` resolve to the dev build and shadow every
released install. `which -a stella` shows only the one entry, which is why it
reads like an alias. There is no `alias stella=` anywhere.

`upgrade` **comments the line out** rather than deleting it, backs the rc file up
first, and **fails closed**: if the line is not present verbatim it edits
nothing. A mangled login shell is not a recoverable mistake and is not worth a
clever regex.

---

## What it does, in order

1. Adds the tap if missing.
2. `brew install` or `brew upgrade` the tap-qualified formula.
3. Backs up `~/.zshrc` to `~/.zshrc.fullauto-<timestamp>.bak`, comments out the
   shadow line, and leaves a comment saying what removed it and why.
4. Unless `--no-dev-alias`, drops in a replacement so the dev build stays one
   word away:
   ```sh
   stella_dev() { "$HOME/Projects/stella/target/release/stella" "$@"; }
   ```
   Nothing is lost — `stella` becomes the release, `stella_dev` is the build you
   were shadowing with.
5. Prints how `stella` resolves in this shell versus a fresh one.

**Open a new terminal (or `exec zsh`) afterwards.** The current shell already
computed its PATH and will keep resolving to the dev build until it restarts —
which looks exactly like the upgrade failing.

## Restore

Every run prints its own restore line. It is always:

```bash
cp ~/.zshrc.fullauto-<timestamp>.bak ~/.zshrc && exec zsh
```

## Verify

```bash
exec zsh
command -v stella          # -> /opt/homebrew/bin/stella
stella --version           # -> the released version, not the dev build's
brew list --versions stella
```

If `command -v stella` still points into `Projects/stella/target/release`, the
shell has not restarted or a second prepend exists — grep the rc files:

```bash
rg -n 'Projects/stella/target/release' ~/.zshrc ~/.zprofile ~/.zshenv
```
