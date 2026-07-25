# Licensing

Stella is **dual-licensed**. You choose which of the two tracks you are on.

| | Open source track | Commercial track |
|---|---|---|
| **License** | [AGPL-3.0-only](LICENSE) | Negotiated commercial license |
| **Cost** | Free | Paid |
| **You must publish your source** | Yes — including over a network | No |
| **Best for** | OSS projects, research, internal evaluation, personal use | Closed-source products, proprietary forks, hosted/SaaS offerings |
| **How to get it** | Just use it | <licensing@oxagen.sh> |

Not sure which track you are on? Read [Do I need a commercial
license?](#do-i-need-a-commercial-license) below, or just ask.

---

## The open source track: AGPL-3.0-only

Stella is free software under the [GNU Affero General Public License, version
3](LICENSE). You may run it, read it, modify it, and redistribute it at no cost.
In exchange the AGPL asks for reciprocity — if you pass Stella on, you pass the
source on too:

1. **Distribute a modified Stella → publish your modifications.** Anyone you
   give a binary to is entitled to the corresponding source, under the AGPL.
2. **Run a modified Stella as a network service → publish your modifications.**
   This is [AGPL §13](LICENSE), and it is the clause that distinguishes the AGPL
   from the ordinary GPL. If your users interact with a modified Stella
   remotely, you must offer them the source of your modified version. Hosting is
   not a loophole.
3. **Combine Stella with your own code → that combined work is AGPL too.** The
   copyleft is not file-scoped. Linking Stella's crates into your application
   makes the result a derivative work.
4. **Keep the notices.** Preserve copyright and license notices, and state what
   you changed.

Version 3 **only**. The "or (at your option) any later version" clause is
deliberately not granted, so the terms cannot be changed out from under either
side by a future FSF publication.

## The commercial track

The AGPL is a poor fit for some entirely legitimate uses. If any of these
describe you, the commercial license exists for exactly this reason:

- You want to embed Stella in a **closed-source product** you ship to customers.
- You want to run a **modified Stella as a hosted or SaaS offering** without
  publishing your modifications.
- Your company policy, customer contracts, or procurement process **prohibits
  AGPL-licensed code**.
- You need **warranty, indemnity, or support terms** the AGPL explicitly
  disclaims.
- You want to keep a **proprietary fork** private.

A commercial license removes the reciprocal obligations above and replaces them
with negotiated terms. Pricing depends on scope and deployment size.

**Contact: <licensing@oxagen.sh>** — include what you want to build and roughly
how you plan to deploy it, and you will get a straight answer about whether you
need a license at all.

### Do I need a commercial license?

You **do not** need one to:

- Use Stella as a developer tool on your own machine, at work or at home,
  including at a commercial company, on proprietary code. *Using* Stella to
  write closed-source software does not make that software AGPL — the AGPL
  covers Stella itself and works derived from it, not the output of running it.
- Run **unmodified** Stella internally, including on servers.
- Evaluate, benchmark, audit, or study it.
- Contribute to it, fork it publicly, or redistribute it under the AGPL.

You **do** need one to:

- Ship Stella, or anything derived from it, inside a product you do not publish
  the source of.
- Offer a **modified** Stella to third parties over a network without publishing
  the modified source.
- Sublicense Stella to your own customers under terms other than the AGPL.

> This table is a plain-language summary for orientation, not legal advice, and
> it does not modify the [LICENSE](LICENSE). Where the two disagree, the LICENSE
> controls. If real money or real risk is involved, talk to your own counsel —
> and to us.

## Why Oxagen can offer both

Oxagen, Inc. holds the copyright in Stella. A copyright holder is not bound by
the terms it offers to others, so it may license the same code to the public
under the AGPL and, separately, to a customer under commercial terms. This is
the same arrangement used by Qt, Grafana, MongoDB, Nextcloud, and many others.

Keeping that ability intact is why contributions require a
[Contributor License Agreement](CLA.md). A contribution made under the AGPL
alone could not be included in a commercially licensed build — one merged pull
request would otherwise permanently remove a file from the commercial track. The
CLA grants Oxagen the right to relicense contributions; **you keep your
copyright** and the code stays open under the AGPL for everyone.

## Relationship to the Context Graph Protocol

The [Context Graph Protocol](https://github.com/macanderson/context-graph-protocol)
is a separate project under **Apache-2.0**, and stays that way.

That split is intentional. A protocol is only worth something if anyone can
implement it, so CGP carries a permissive license with an explicit patent grant
and no reciprocal obligations — adopt it in anything, including closed-source
software, with no involvement from us. Stella is the reference *implementation*,
and that is where the reciprocity applies.

Depending on CGP does **not** put your project under the AGPL.

## Prior releases

Releases up to and including **v0.5.14** were published under `MIT OR
Apache-2.0`. That grant is perpetual and irrevocable for the versions it was
made under. Anyone who received those releases keeps their MIT/Apache rights in
that code, and a relicense cannot claw them back — including the right to fork
from that point. The AGPL applies to versions after v0.5.14.

## Reporting a license violation

If you believe someone is shipping Stella in violation of the AGPL, email
<licensing@oxagen.sh>. The goal of enforcement is compliance, not litigation:
the usual outcome is that the user either publishes their source or buys a
commercial license.
