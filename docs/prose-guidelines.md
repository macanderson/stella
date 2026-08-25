---
id: prose-guidelines
title: "Prose guidelines"
status: living
---

# Prose guidelines

Rules for every word a human reads in a codebase: README pages, design
documents, doc comments, module headers, commit messages, pull request
descriptions, error strings, and the banner at the top of a shell script.

They are written to be copied. Nothing here depends on this repository, its
language, or its tooling. Take the file, drop it in your own `docs/`, and
point your contributor guide at it.

---

## The one rule

**Write the thing. Do not announce that you are going to write it.**

Everything below is that rule applied to a specific habit.

Prose that carries no information is a defect, in the same way an unused
variable is a defect. It costs the reader time, it hides the sentence that
does carry information, and it grows without limit because no compiler
rejects it.

---

## The test: delete the clause

Before you keep a sentence, cut it and read what is left.

If the reader lost nothing, the sentence was carrying nothing, and it goes.

```
Two things stated rather than hidden: the cache is keyed by path,
and a miss is a hard error.
```

Cut the opening clause and the sentence still says the cache is keyed by
path and a miss is a hard error. So the opening clause goes:

```
The cache is keyed by path. A miss is a hard error.
```

That is the whole method. Apply it to every sentence you are about to ship.

---

## What to cut

### 1. Announcing a list instead of writing it

Phrases like `Two things follow`, `Both halves matter`, `Three reasons to
know`. A reader can count. Write the list.

- **Before:** `Three reasons this runs after the merge, not before.`
- **After:** `This runs after the merge because …` (then the reasons)

### 2. Telling the reader which item to care about

Phrases like `the part that matters`, `and the second is the hard one`,
`this is the important bit`. If one item matters more, put it first. Order
is how you rank things in prose.

- **Before:** `The parser has four passes; the third is the one that matters.`
- **After:** `The third pass resolves names.` (then the other three)

### 3. Prose about the prose

Phrases like `stated rather than hidden`, `worth naming`, `it bears
repeating`, `to be clear`. These describe the act of writing instead of
saying anything. State the thing.

### 4. The rhetorical contrast with a made-up opponent

The `X, not Y` tail where nobody proposed `Y`. `A contract, not
decoration.` `A feature, not a bug.` The claim stands on its own; the tail
is applause.

- **Before:** The schema is checked at build time, not just documented.
- **After:** The schema is checked at build time.

### 5. Tired metaphor standing in for the plain word

Say what the thing does.

| Instead of | Write |
|---|---|
| `load-bearing` | required |
| `belt and braces` | checked twice |
| `first-class citizen` | supported |
| `single source of truth` | the only copy |
| `battle-tested` | in production since *(date)* |
| `under the hood` | internally |

### 6. Declaring that you are telling the truth

`the honest number`, `to be honest`, `truthfully`. A reader assumes you are
telling the truth. Announcing it invites the question of when you were not.
State the fact.

### 7. Filler adverbs

`deliberately`, `simply`, `just`, `basically`, `essentially`, `actually`,
`of course`, `obviously`, `note that`.

`deliberately` is the one that hides best, because it sounds like it is
carrying a reason. It is not. Either the reason follows, in which case the
word adds nothing to it, or no reason follows, in which case the word is
standing in for one.

- **Before:** This is deliberately a source scan rather than a runtime check.
- **After:** This is a source scan rather than a runtime check, because the
  defect is which call site was used, which no runtime check can see.

`simply` and `just` are worse than empty: they tell a reader who is stuck
that the thing they are stuck on is easy.

### 8. Insider vocabulary

A word a new reader cannot look up is a word that stops them. Replace
project nicknames, internal codenames, and language-specific terms with the
plain word.

Say *option* rather than a language's word for a case in a list of cases.
Say *rule* rather than a maths word for something that stays true. Say
*interactive mode* rather than the nickname your team gave the screen.

Names of files, types, and commands stay as they are — write them in
backticks so a reader can tell a name from a word.

### 9. History in a place a reader is not looking for it

A module header explains what the file does today. It is not where the
story of how the file got here belongs.

Move the history to the pull request that made the change, to a design
document, or to the issue tracker. What stays in the source is the part a
reader needs in order to change the file safely.

- **Keep:** what this file does, what it must not do, where the boundary is.
- **Move:** which pull request changed it, what it used to look like, the
  incident that prompted it, the argument that was had about it.

An exception: a warning that stops the next person repeating a mistake earns
one sentence, in the present tense, saying what not to do and why.

### 10. A number that will be wrong next month

`Seventeen tokens`, `twenty-five crates`, `four callers`. The code changes;
the sentence does not. Every count written in prose is a promise to update
it, and nobody does.

Point at the list instead: *the entries in `token::ALL`*. If a count really
has to be stated, put it somewhere a test can check.

Line numbers are the same defect, sharper. A citation like
`src/fleet.rs:463` is wrong after the next edit above line 463. Cite the
name — `fleet.rs`'s `dispatch` — which survives the edit and is what a
reader searches for anyway.

---

## Tense: say what is true now

The most damaging prose defect is not bloat. It is a sentence that describes
something that is no longer so, written in the present tense, so a reader
believes it.

Three ways this happens:

- **A plan written as a fact.** Someone decides to change a design, writes
  the decision into the source in the present tense, and does not make the
  change. Write a decision that has not shipped in the future tense, and name
  where it is tracked.
- **A deletion nobody followed.** A file is removed and the sentences
  pointing at it stay. If you delete or rename something, search the whole
  repository for its name in the same change.
- **A count that drifted.** See rule 10.

If you edit a file, you own every sentence in it. A stale sentence you walked
past is now yours.

---

## Length

There is no word limit. There is a content bar: every sentence has to do
work.

Two rules of thumb that hold up in practice:

- A module or file header is two to four sentences: what this file does, and
  what a reader must not do to it. Longer than that and it is a design
  document living in the wrong place.
- A doc comment on a function says what it returns and what it refuses. If
  the body makes both obvious, write nothing.

Never write a comment that narrates the next line.

---

## Reading level

Aim for prose a fourteen-year-old can follow. This is not about dumbing
anything down — the ideas stay as hard as they are. It is about not adding a
second difficulty on top of the first.

Concretely: short sentences, ordinary words, one idea per sentence, the
subject near the verb.

---

## Enforcing it

Prose rots the way code rots, so it needs the same kind of gate. A checker
that greps for the banned phrases and fails the build catches most of this,
and it needs three properties to survive contact with a real repository:

1. **It only ever goes down.** Record the count that already exists per
   file, refuse to raise it, and refuse to add a file that was clean.
   Existing debt is grandfathered; new debt fails.
2. **It cannot be silenced by editing the record.** The way past a failure
   is to delete the prose. If a contributor can pass by raising a number,
   they will.
3. **A quoted phrase is allowed.** A document that bans a phrase has to be
   able to spell it, so skip anything inside backticks or a fenced block.

The first time you turn such a checker on you will find thousands of hits.
That is the point. Record them, then take the count down.

---

## A checklist for review

- [ ] Cut every clause that costs the reader nothing when deleted.
- [ ] No sentence describes something that is no longer true.
- [ ] No counts, no line numbers, no other promise to update later.
- [ ] Module headers are two to four sentences about what the file does.
- [ ] History lives in the pull request, not the source.
- [ ] Every name a reader cannot look up is replaced or defined.
- [ ] Read it out loud. Anything you would not say to a colleague, cut.
