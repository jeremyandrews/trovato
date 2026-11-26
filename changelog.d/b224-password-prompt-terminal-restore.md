`trovato user create` and `trovato user reset-password` left the terminal
with echo off if Ctrl-C, or a `SIGINT`, `SIGTERM` or `SIGHUP`, arrived while
`Password:` was waiting for an answer: the read turned echo off before
printing the prompt and back on after the line came in, but the restore ran
only on that normal return, so a signal landing in between killed the process
first and left the shell showing nothing for what was typed, until `stty
echo` put it back. The one function both commands use to read a password now
saves the terminal's settings, installs SIGINT, SIGTERM and SIGHUP handlers
for exactly as long as it is waiting on the read, and uninstalls them again
once it returns. A signal in that window restores the terminal itself, prints
a newline and exits with the same code the shell would have reported had it
not been caught (130 for SIGINT); a panic during the read restores it too.
Piped input, and a password typed with no interruption, behave exactly as
before.
