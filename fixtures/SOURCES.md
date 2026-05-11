# Fixture Sources

Each fixture file is either drawn from a well-known source or uses a
standardized real-world format. Attribution below.

## words.txt
Curated English word list. Mix of common nouns, proper nouns, technical
terms, and words with interesting text-processing properties (hyphens,
apostrophes, accented characters, varying lengths). Not drawn from a
single source — assembled from general English vocabulary knowledge.

## naughty.txt
Selected sections from the Big List of Naughty Strings (BLNS).
Source: https://github.com/minimaxir/big-list-of-naughty-strings
License: MIT (Copyright (c) 2015-2020 Max Woolf)
Sections included: Numeric Strings, Special Characters, Unicode Symbols,
Two-Byte Characters, Right-to-Left Strings, Emoji, Zalgo Text.

## access_log.txt
Apache HTTP Server Combined Log Format.
Format spec: https://httpd.apache.org/docs/2.4/logs.html
All entries are fabricated (no real IPs or paths) but follow the exact
format specification. Timestamps use realistic spreads.

## data.csv
Fabricated tabular dataset in RFC 4180 CSV format.
Format spec: https://tools.ietf.org/html/rfc4180
Contains header row, mixed types (text, numeric, dates), quoted fields
with commas, empty fields, and duplicate rows.

## passwd.txt
/etc/passwd format (colon-delimited, 7 fields).
Format spec: passwd(5) man page.
All entries are fabricated users with realistic UIDs, shells, and home
directories. No real system accounts.

## syslog.txt
BSD syslog format (RFC 3164) entries.
Format spec: https://tools.ietf.org/html/rfc3164
Fabricated entries with realistic timestamps, hostnames, programs, PIDs,
and message content.

## dates.txt
Date/time strings in various real-world formats.
Includes ISO 8601, RFC 2822, Unix timestamps, locale-formatted dates,
month names, and ambiguous date formats.

## paths.txt
Realistic Unix filesystem paths.
Mix of absolute, relative, dotfiles, spaces in names, symlink targets,
deep nesting, and special characters.

## formatted.txt
Text with whitespace edge cases for tools like cat -v, fold, fmt, nl.
Includes tabs, trailing whitespace, blank lines, control characters,
ANSI escape sequences, mixed indentation, and very long lines.

## numbers.txt
Numeric strings in various formats.
Includes integers, floats, scientific notation, negative numbers,
locale-formatted numbers (1,000.00 vs 1.000,00), hex, octal, and
edge cases (NaN, Infinity, -0).
Numeric section drawn from BLNS (MIT license, Copyright (c) 2015-2020 Max Woolf).
