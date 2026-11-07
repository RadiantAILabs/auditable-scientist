#!/bin/sh
# Stand-in for bibtex: records the call and writes a one-entry bibliography.
# A paper that asks for `% fake: quiet` (see fake_pdflatex.sh) gets no record.
set -u

if ! grep -q '^% fake: quiet$' paper.tex; then
  printf 'bibtex %s\n' "$*" >> fake_calls.txt
fi
printf '\\begin{thebibliography}{1}\n\\bibitem{x} Entry.\n\\end{thebibliography}\n' > paper.bbl
echo "This is FakeBibTeX" > paper.blg
exit 0
