Binary-Validated Man Pages

Generate accurate, comprehensive man pages by validating existing documentation against the observable behavior of a binary.

This project treats the executable on disk as the source of truth. Existing man pages, --help output, and source code are treated as claims about the binary, which are then systematically validated through direct execution and controlled experiments.

The result is a regenerated man page that documents what the binary demonstrably does, explicitly marks unknowns, and surfaces discrepancies with existing documentation.

Motivation

Man pages are the primary interface between humans and command-line tools, but in practice they often:

drift from actual binary behavior,

omit default behaviors and filtering rules,

disagree across versions or distributions,

encode assumptions that are no longer true.

When documentation is wrong or incomplete, both humans and language models must guess. This project reframes the problem:

Instead of writing documentation by hand, can we mechanically validate what existing documentation claims, and regenerate a man page that matches the observed behavior of the binary?

Goal

The goal of this project is to produce a regenerated man page whose statements are backed by observed behavior of a specific binary artifact.

For a given binary:

Existing man pages and --help output are parsed into a set of documentation claims.

Each claim is tested against the binary using controlled inputs and environments.

Claims are classified as:

Confirmed — matches observed behavior

Refuted — contradicted by observed behavior

Undetermined — could not be validated with available tests

The generated man page includes:

confirmed behaviors,

corrected descriptions where claims are refuted,

explicit marking of undetermined behavior.

This project focuses on documentation fidelity, not on modifying or controlling tool behavior.

What “Comprehensive” Means

In this project, comprehensive has a precise, operational definition.

A generated man page is considered comprehensive if every user-visible behavior of the binary is either:

documented and validated, or

explicitly marked as undetermined.

Concretely, comprehensiveness requires all of the following.

1. Surface Completeness (Invocation Space)

The man page documents all behaviors reachable through:

command-line flags and options,

accepted argument forms,

environment variables that materially affect behavior,

stdin / stdout / stderr usage.

Requirements:

Every option accepted by the binary must appear.

Default behavior must be explicit.

Observable precedence or mutual exclusion between options must be documented.

Accepted but no-op or alias options must be identified.

Rejected or invalid options must be excluded.

If a user can invoke it and observe a behavioral difference, it belongs in the man page.

2. Behavioral Completeness (Effects)

For each documented invocation mode, the man page describes:

what kinds of entities the tool operates on,

what is included in output,

what is omitted or filtered,

what classes of errors can occur,

what exit statuses mean.

This does not require enumerating all possible cases. It requires identifying policies, defaults, and categories of behavior.

3. Observational Grounding

Every documented statement must be:

directly observed via execution, or

supported by repeated empirical observation under controlled conditions, or

explicitly marked as undetermined.

Internally, claims must be traceable to:

execution logs,

exit codes,

output diffs,

or recorded gaps in coverage.

A comprehensive man page includes the boundaries of knowledge, not just confirmed facts.

4. Negative-Space Completeness (Limits and Unknowns)

The generated man page explicitly documents:

behaviors that are unspecified or environment-dependent,

areas where behavior varies by filesystem, locale, or platform,

cases that were not observed or could not be conclusively tested.

Examples:

“Behavior with invalid UTF-8 input is unspecified.”

“Output order is not guaranteed unless sorting options are provided.”

“Some errors may vary by filesystem or permissions.”

Unknowns are first-class documentation, not omissions.

Binary as Source of Truth

All generated documentation is tied to a specific binary artifact.

Each run records:

absolute path to the binary,

cryptographic hash of the binary contents,

platform and minimal environment metadata.

Generated man pages are valid only for that binary version.

If documentation contradicts observed behavior, the binary wins.

Documentation as Claims

Existing documentation artifacts are treated as non-authoritative claims, including:

man <tool>

<tool> --help

optional excerpts from source code

These claims answer:

“What does the documentation say should happen?”

They are never assumed to be correct without validation.

Validation and Regeneration

Claims are validated by executing the binary against controlled fixtures and constrained environments:

filesystem fixtures for file-oriented tools,

forced locale and terminal settings,

restricted input domains.

Claims are:

confirmed,

refuted (with corrected behavior described),

or marked undetermined.

The regenerated man page is assembled from validated claims and explicit unknowns.

Output Artifacts

For each binary, the project produces:

A regenerated man page (man(1) format), including:

NAME

SYNOPSIS

DESCRIPTION

OPTIONS

BEHAVIOR

ERRORS

EXIT STATUS

NOTES / CAVEATS (including unknowns)

A validation report (machine-readable):

list of claims and their status,

evidence used for validation,

discrepancies with original documentation,

explicit gaps in coverage.

The man page is usable by humans without additional tooling.
The report exists for auditability and debugging.

Scope and Stopping Conditions

This project is intentionally conservative.

Not all behaviors can be validated exhaustively.

Probing stops when:

surface completeness is achieved, and

remaining claims are environment-dependent or intractable.

Undetermined behavior is explicitly documented rather than guessed.

Initial Scope

The initial target is a single coreutils-style binary (e.g. ls), chosen for:

well-defined invocation semantics,

existing but imperfect documentation,

non-trivial option surface.

The first milestone focuses on:

validating option existence and argument arity,

validating a small set of stable default behaviors,

surfacing at least one documented discrepancy.

Evaluation Criteria

A regenerated man page is successful if:

it documents only behaviors the binary actually exhibits,

default behaviors are explicit,

at least one discrepancy with original documentation is justified,

undetermined behavior is clearly marked,

the document is tied to a specific binary hash/version.

Correctness and transparency take priority over breadth.

Forward-Looking Relevance

A binary-validated man page:

reduces reliance on outdated documentation,

provides a trustworthy context artifact for humans,

serves as a minimal, high-signal input for language models.

This project deliberately stops at documentation generation, but is designed to support downstream work.

License

MIT.

Design Invariant

A man page is comprehensive if every user-visible behavior of the binary is either documented or explicitly marked as undetermined.

