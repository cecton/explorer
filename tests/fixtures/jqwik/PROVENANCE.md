Excerpts of jqwik, kept byte for byte as a scan fixture.

They are the files that carry jqwik's anti-AI notice, the case a repository scanner
cannot find by looking for hidden characters: the notice is plain text, and the escape
sequence that hides it from a terminal is written as a source literal that only becomes
an escape byte when the test engine prints it.

Upstream: https://github.com/jqwik-team/jqwik
Commit:   5c2ac9acfb8dc9a246c774094652b0f089ab90d3
License:  Eclipse Public License 2.0

- engine/JqwikExecutor.java          net/jqwik/engine/execution/JqwikExecutor.java,
                                    excerpted to the class and its notice, imports and
                                    collaborators removed
- documentation/how-to-use.md        documentation/src/docs/include/how-to-use.md,
                                    the "how to use" note on the anti-AI clause
- .noai                              the repository root's note to agents; hidden, so it
                                    also pins that the walk reaches dotfiles

The scanner's rules must report the notice as `review` prose and never as `danger`: the
repository hides nothing, it says so out loud.
