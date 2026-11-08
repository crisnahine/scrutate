# Perl: no grammar, heuristic symbols
use strict;
package Demo::Greeter;
# Greets.
sub greet {
    my ($name) = @_;
    return "hi $name";
}
sub helper { return 2 * shift }
1;
