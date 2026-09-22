# cstore contributor contract

cstore owns portable storage contracts and backend implementations. Product
schemas, editorial rules and authorization remain in consumers. Preserve opaque
data and return explicit errors instead of silently weakening storage guarantees.

Read `docs/ccvl-filesystem.md` before changing file mapping or transfer fixtures.
CCVL is an existing consumer with an established layout; never make it adopt a
generic object directory merely to simplify a backend.

Write safe Rust. Use current stable Rust and retained dependency locks. Test
changes through the repository's ccid checks using GitHub Actions or the existing
Crow fallback; workstations orchestrate rather than build. Do not install tools on
owned workers. Keep test inputs neutral and free of private workspace content.

Commit explicit paths directly to main. Preserve concurrent work, license notices
and source ownership. Do not add AI attribution to commits.

