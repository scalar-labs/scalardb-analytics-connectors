# Prints the section of one version in CHANGELOG.md, without its heading, as
# the notes of that version's GitHub Release (.github/workflows/package.yml).
# A list item's wrapped lines are joined into one, because a release body
# shows each line break.
#
# Usage: awk -v heading='## [<version>]' -f release-notes.awk CHANGELOG.md
#
# Prints nothing when the changelog has no section for the version.

index($0, "## [") == 1 { if (found) exit; found = index($0, heading) == 1; next }
!found { next }
# The link reference definitions that end the changelog.
/^\[[^]]+\]: / { exit }
/^ +[^ ]/ && line != "" && $0 !~ /^ *[-*] / { sub(/^ +/, " "); line = line $0; next }
{ if (started) print line; else if (line != "") { started = 1; print line } line = $0 }
END { if (line != "") print line }
