# Spec: make umbriel-mcp publishable (0.1.0)

## Problem Statement

`umbriel-mcp` works, but only on the machine it was written on. Anyone else who tries to
use it hits a wall before they reach a single tool call.

The server drives pointer and keyboard input through `ydotool`, which needs a background
daemon, access to the `uinput` device, membership of a privileged group, and a re-login.
That setup is different on every distribution and broken out of the box on several: on one
major distro the packaged service runs as root with a socket ordinary users cannot reach,
on another the shipped unit fails to start because no device rule is installed, on two
others the tool is not packaged at all, and on an older long-term-support release the
command of that name is an entirely different program with no daemon. Joining the
privileged input group is also a real security downgrade, because it grants read access to
every input device on the system.

So a new user's first experience is a permission error, and the fix depends on facts about
their distribution that the project cannot reasonably document.

On top of that, nothing about the project is ready to be consumed by strangers. There is no
release artifact, no continuous integration, no way to discover it, and no guidance on
wiring it into an AI coding harness — where every harness uses a different configuration
dialect. The server also exposes arbitrary command execution through its compositor-action
tool, with no way to run it with less authority and no statement anywhere of what an agent
holding it can actually do.

## Solution

Remove the privileged dependency entirely, then ship the result properly.

The compositor already implements the Wayland protocols needed to synthesise pointer and
keyboard input from an ordinary unprivileged client. Speaking those protocols directly
deletes the daemon, the device rule, the group membership, and the re-login — and is more
accurate as well, because it is unaffected by pointer acceleration. Clipboard access moves
to a library for the same reason. Screen capture and optical character recognition keep
shelling out to external programs, because those need no privileges and reimplementing
capture correctly is a large job with a subtle failure mode.

After that change the only external programs are two that exist in the official
repositories of every distribution the compositor runs on, so the install story becomes
"download a single static binary and run it."

Alongside that: three capability profiles so the server can be run with less authority than
full control; a diagnostic command that turns any install problem into one command; a
helper that registers the server with an AI coding harness; static release binaries for the
two architectures the compositor supports; and continuous integration to keep it building.

## User Stories

1. As a new user, I want to install the server without joining a privileged system group, so that I do not have to weaken my machine's security to use it.
2. As a new user, I want to install the server without configuring a background daemon, so that installation is a single step.
3. As a new user, I want to install the server without re-logging in, so that I can try it immediately.
4. As a new user, I want a single self-contained binary, so that I do not have to match my distribution's package versions.
5. As a user on a distribution with no package for the old input helper, I want the server to work anyway, so that my choice of distribution does not exclude me.
6. As a user on an older long-term-support release, I want to be told plainly that my system is unsupported, so that I do not waste an afternoon discovering it.
7. As a user, I want to know which compositor versions and distributions are supported before installing, so that I can judge whether this is for me.
8. As a user, I want a diagnostic command that checks every prerequisite, so that I can fix a broken install without reading source code.
9. As a user, I want the diagnostic to name the exact package to install for my distribution, so that I do not have to translate package names myself.
10. As a user, I want the diagnostic to tell me whether the compositor is reachable, so that I can distinguish "server is broken" from "compositor is not running".
11. As a user, I want the diagnostic to tell me whether the input protocols are available, so that I understand why input tools fail under a restricted session.
12. As a user whose optical character recognition language data is missing, I want to be told that specifically, so that I do not conclude the whole feature is broken.
13. As a user, I want tools that depend on a missing program to fail with an explanatory message when called, rather than silently disappearing, so that I understand what is wrong.
14. As a user, I want to register the server with my AI coding harness using one command, so that I do not have to hand-write configuration.
15. As a user of a harness with no command-line interface, I want a configuration snippet I can paste, so that I am not stuck.
16. As a user, I want the registration helper to never rewrite my existing configuration file, so that my comments and unrelated settings survive.
17. As a user, I want the registration helper to tell me any follow-up step the harness requires, so that I am not left wondering why nothing appeared.
18. As a cautious user, I want to run the server without the ability to execute arbitrary commands, so that I can limit what an autonomous agent can do.
19. As a cautious user, I want to run the server in a read-only mode, so that I can let an agent observe my desktop without touching it.
20. As a cautious user, I want the restricted modes to be genuinely restrictive rather than cosmetic, so that they cannot be trivially bypassed through a general-purpose tool.
21. As a user, I want the default mode to be fully capable, so that the common case needs no flags.
22. As a user, I want the documentation to state plainly what an agent holding this server can do to my machine, so that I can make an informed decision.
23. As an agent, I want to list windows with their identity, title, geometry, output, and workspace, so that I can decide what to act on.
24. As an agent, I want to list workspaces per output, so that I can navigate a multi-monitor layout correctly.
25. As an agent, I want to query the focused window, so that I can act on what the user is looking at.
26. As an agent, I want to switch to a workspace on a named output, so that I can target a specific monitor rather than guessing.
27. As an agent, I want to focus a window by identity, so that I do not depend on directional guessing.
28. As an agent, I want to move the pointer to an absolute screen position accurately on a multi-monitor layout, so that my clicks land where I intend.
29. As an agent, I want to click, scroll, and drag, so that I can operate interfaces that have no other automation surface.
30. As an agent, I want to type arbitrary text including non-Latin characters, so that I am not limited to the user's keyboard layout.
31. As an agent, I want to press key chords with modifiers, so that I can trigger application shortcuts.
32. As an agent, I want to capture the whole desktop, one output, or one window, so that I can look at the right thing without wasting context on the rest.
33. As an agent, I want captured images downscaled with a stated coordinate mapping, so that I can convert what I see back into screen coordinates.
34. As an agent, I want to find text on screen and receive absolute screen coordinates, so that I can act on what I read.
35. As an agent, I want to click a piece of text directly, so that I can operate an interface that exposes no other handle.
36. As an agent, I want to read and write the clipboard, so that I can move data between applications.
37. As an agent, I want a general-purpose tool for any compositor action, so that I am not blocked when no dedicated tool exists.
38. As an agent, I want tool errors to explain the cause, so that I can recover rather than retry blindly.
39. As an agent, I want the server to describe its own purpose and tool set, so that I can use it without external documentation.
40. As a maintainer, I want the input path covered by an automated test that needs no compositor, so that I can refactor without a desktop session.
41. As a maintainer, I want a test that pins absolute pointer accuracy against a live compositor, so that the multi-monitor coordinate bug cannot return unnoticed.
42. As a maintainer, I want tests that exercise the server through its real external interface, so that I am testing the contract rather than internals.
43. As a maintainer, I want continuous integration to build, lint, and format-check on every change, so that obvious breakage never reaches a user.
44. As a maintainer, I want continuous integration to build the static artifact on every change, so that a dependency change cannot silently break releases.
45. As a maintainer, I want tagging a version to produce downloadable binaries automatically, so that releasing is not a manual ritual.
46. As a maintainer, I want publishing to the package registries to stay manual, so that no publishing credentials live in a public repository.
47. As a maintainer, I want the dependency set pinned so the static build stays free of native code, so that a future feature flag cannot quietly break portability.
48. As a maintainer, I want the project scoped to one compositor, so that I am not maintaining abstractions over window models that do not match.
49. As a maintainer, I want expectations set as a best-effort personal project, so that publishing does not create a support obligation.
50. As a potential user, I want to discover the server through the ecosystem registry, so that I can find it without knowing it exists.
51. As a potential user, I want to install it through my distribution's user repository, so that it updates alongside my other packages.
52. As a contributor, I want a packaging recipe in the repository, so that I can adapt it for my own distribution.
53. As a contributor, I want to see which behaviours are covered by tests, so that I know what I might break.

## Implementation Decisions

**Scope.** This release targets one compositor only. No abstraction layer over other
compositors will be introduced, even though most of the non-compositor code is portable.
Their window models differ enough — scrolling strips, tiling trees, tag sets — that a
shared abstraction would be inaccurate exactly where an agent needs precision. The module
boundary around compositor communication stays clean so this can be revisited, but no
backend trait is built speculatively.

**Input backend.** Pointer and keyboard synthesis moves from external command invocation to
the Wayland virtual-pointer and virtual-keyboard protocols, consumed through an existing
third-party crate that already implements them. The previous external input dependencies
are removed outright rather than kept as a fallback; keeping them would preserve exactly
the install friction this change exists to delete.

**Absolute pointer coordinates.** This is the single highest-risk detail. The protocol
request takes an extent alongside the coordinates, and the extent must be the **entire
desktop layout**, not the size of any individual output. Passing an output when creating
the virtual pointer does not constrain the mapping to it. A prototype against a live
dual-output session produced this, and it is the reason the rule is stated explicitly:

```
layout: two 1920x1080 outputs side by side, global layout 3840x1080

extent = per-output (1920x1080):   requested (100,100) -> observed (200,100)   WRONG
extent = global layout (3840x1080): requested (100,100) -> observed (100,100)   correct
```

Coordinates are global desktop coordinates throughout the public tool surface.

**Clipboard.** Moves from external command invocation to a library. This also removes a
latent defect in the current implementation, where reading the child process output to end
of stream blocked for a full timeout because the clipboard process holds the stream open.

**Screen capture and text recognition.** Both keep shelling out. Neither needs privileges,
so neither contributes install friction, and reimplementing capture correctly means
handling buffer formats, output transforms, fractional scaling, and multi-output
composition — a large job whose failure mode is a subtly wrong image that silently degrades
text recognition. Capture is the only remaining hard external dependency; text recognition
is required only by the three tools that use it.

**Capability profiles.** Three, selected by a command-line flag, defaulting to full
capability:

| Profile | Permits |
|---|---|
| read-only | queries, screen capture, clipboard read |
| no-exec | the above, plus window and workspace control and input synthesis |
| full | everything, including launching programs |

The general-purpose compositor-action tool is an arbitrary-code-execution path, because the
compositor's action vocabulary includes spawning a command. Under `no-exec` that tool must
reject any action in the spawn family; gating only the dedicated launch tool would leave a
trivial bypass. Profile enforcement happens at tool-invocation time, and tools remain
advertised in all profiles so a client receives an explanatory refusal rather than silence.

**Degraded operation.** Tools are always advertised regardless of whether their external
program is present. A missing program produces an error at call time naming the required
package for the detected distribution. Hiding tools at startup is rejected: it denies the
client any way to learn why a capability is absent.

**Diagnostic command.** A subcommand checks, and reports on, compositor socket reachability,
availability of the virtual-pointer and virtual-keyboard protocol globals, the capture
program, and the text-recognition program *including its English language data* — the last
being a real failure mode already observed, where the program is installed but unusable.
The protocol-global check exists because a restricted Wayland session can filter those
globals out, which would otherwise present as an unexplained input failure.

**Harness registration.** A subcommand registers the server with a named AI coding harness.
It delegates to each harness's own command-line interface where one exists, and otherwise
prints a configuration snippet for the user to paste. It never parses, edits, or rewrites a
user's configuration file. This is a deliberate limit: research across six harnesses found
four mutually incompatible configuration dialects and three different environment-variable
syntaxes, with at least one harness having changed schema recently and another having two
generations in circulation. Owning writers for all of them means inheriting everyone's
schema churn; delegating pushes that cost back to the parties who control it. Two
harness-specific details are handled because they otherwise read as a broken install: one
harness defaults to an exposure mode where tools are not offered to the model directly and
must be opted out of, and another fails to parse its entire settings file if an empty
argument list is omitted. The subcommand accepts a profile option and passes it through
into the generated invocation, defaulting to none so that registration does not silently
restrict capability.

**Distribution.** Primary artifact is a statically linked binary for the two architectures
the compositor supports, attached to tagged releases. A prototype build confirmed the whole
dependency tree is free of native code and produces a position-independent static
executable around one and a half megabytes with no runtime dependencies. The dependency on
the protocol framework must have its feature set pinned, because some of its optional
features pull in native cryptography; none of those features are needed for a
standard-stream server. Secondary channels are the language package registry and a binary
package in the distribution user repository. A source packaging recipe stays in the
repository as a starting point for contributors but is not itself published.

**Support statement.** No support tier table. The only external programs are available
everywhere the compositor runs, so the limiting factor is the compositor's own
availability, and any tier list would be a stale copy of its installation page. Two
specific warnings are documented because users will otherwise hit them: the compositor is
packaged for only the most recent release of one major distribution family, and that
distribution's user repository contains a placeholder package with a confusingly similar
name that must not be installed.

**Continuous integration.** One workflow. On every change: format check, lint with warnings
denied, standard build, static build. On a version tag: build both architectures, generate
checksums, create the release. Publishing to the package and ecosystem registries stays
manual, both to keep publishing credentials out of a public repository and because the
ecosystem registry verifies ownership by reading the rendered page on the package registry,
making the ordering between them awkward to automate reliably.

**Project posture.** Published as an explicitly best-effort personal project under an
individual account rather than donated to the compositor's organisation. Documentation
states that it tracks a pre-release compositor and that breakage is expected.

## Testing Decisions

**What makes a good test here.** Tests assert externally observable behaviour: what a client
receives when it calls a tool. They do not assert on internal structure, module boundaries,
or the shape of intermediate values. A test should survive the input backend being replaced
wholesale, which is precisely the change this spec makes — the existing manual verification
already did survive it, which is the strongest argument for pinning the seam where it is.

**No prior art exists.** The project currently has no tests of any kind, no test directory,
and no testing conventions. This spec establishes the pattern. There is also no project
glossary and no architecture decision records, so the vocabulary used throughout is the
compositor's own — output, workspace, window identity, action — rather than an invented
one.

**Primary seam: the server's standard-stream protocol boundary.** Tests launch the real
built binary as a subprocess, perform the protocol handshake, call tools, and assert on the
results. Two hooks that already exist in production code make this possible without
introducing any abstraction for testability: the compositor socket location is read from an
environment variable, and the external programs are resolved through the executable search
path. A test therefore substitutes a fake compositor — a socket server replying with canned
responses — and fake external programs placed earlier on the search path.

This one seam covers tool registration and advertised schema, request and reply handling
against the compositor including error mapping, window and workspace filtering, the capture
pipeline and its coordinate mapping, text-recognition output parsing, profile enforcement
including the spawn-family rejection, and the degraded-operation messages when an external
program is absent.

It is the highest available seam because it is the actual contract consumers use, and
preferring it keeps the number of seams in the codebase at one.

**Secondary seam: live compositor smoke tests, excluded by default.** A small set of tests
marked to run only on explicit request, against a real compositor session. These exist for
what cannot be faked — input synthesis and clipboard access, both of which talk to a real
display server. Their most important assertion is that a request to move the pointer to an
absolute position results in that exact position being observed, on a multi-output layout.
That is the regression test for the coordinate bug described in the implementation
decisions, and it is the reason this second seam is accepted at all.

**Rejected: an abstraction over the input and capture backends.** Introducing a trait so
these could be mocked would add a second seam in the production code and indirection at
every call site, in exchange for tests that cannot catch the only bug class that actually
threatens this area — incorrect interaction with the real display server protocols. The
live smoke test covers that risk directly.

**Ordinary unit tests** cover pure functions with no input or output: key-chord parsing into
modifiers and key, the spawn-family prefix check, image-to-screen coordinate conversion, and
geometry formatting. These require no test doubles and introduce no seam.

**Not covered by automated tests**, and accepted as manually verified: the harness
registration subcommand's interaction with third-party command-line tools, and the
diagnostic subcommand's per-distribution package naming.

## Out of Scope

- Support for any compositor other than the target one, including any backend abstraction built in anticipation of it.
- Reimplementing screen capture or text recognition in-process.
- Keeping the previous external input programs as a fallback path.
- Distribution packages for formats beyond the one user repository named above; the source recipe in the repository is a contributor starting point, not a published artifact.
- Bundled installation formats for consumer desktop applications. These were evaluated and reach only a minority of the relevant harnesses.
- Automated publishing to the package or ecosystem registries.
- Support for the older long-term-support releases where the compositor itself is unavailable.
- Reading or writing users' harness configuration files programmatically.
- A permission or approval prompt layer around destructive tools; the capability profiles are the whole of the safety model for this release.
- Any change to the set of tools offered. The tool surface is unchanged by this work.

## Further Notes

The prototype work behind this spec was validated against a live session of the target
compositor: the input protocols were confirmed present and exercised unprivileged, absolute
positioning was confirmed exact once the extent rule was understood, a frame was captured,
and a synthetic keymap including an emoji code point was accepted. The static build was
confirmed by building the real dependency set. These are not assumptions.

The text-recognition dependency has never actually functioned on the development machine,
because the English language data package is not installed. Those three tools are therefore
unverified end to end. The language data must be installed and the tools exercised before
release, or they should be cut — shipping a feature that has never once run is worse than
not shipping it.

The replacement of a working, manually verified input path is the main risk in this work.
The current implementation functions correctly on the development machine; the change trades
that known-good state for a substantially better experience for everyone else. The effort
estimate assumes the third-party input crate exposes what the prototype suggests. If it does
not, the fallback is implementing the protocols directly, which the prototype also proved
viable but roughly doubles the cost of that phase.

Finally, one observation from the research that is outside this spec's scope but worth
recording: several AI harness configuration files on the development machine contain
credentials in plain text, two of them world-readable. Every one of those harnesses supports
environment-variable interpolation for exactly this purpose.
